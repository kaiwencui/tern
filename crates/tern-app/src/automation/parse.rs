//! Reading requests: a line split into tokens, then a Command.

use std::path::PathBuf;

use tern_types::PaneId;

use super::keys::{modifier_key, parse_key, parse_mods, KeySpec};
use crate::input::{DropPayload, MouseKind};
use crate::menu::AppCommand;
use crate::state::CursorStyle;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Command {
    State,
    Menu,
    MenuClick(AppCommand),
    Key(KeySpec),
    /// A modifier key pressed and released on its own: its key code and
    /// the flags it sets.
    Modifier {
        code: u16,
        flags: usize,
    },
    Text(String),
    Mouse {
        kind: MouseKind,
        x: f64,
        y: f64,
        button: u8,
        flags: usize,
        clicks: u8,
    },
    /// A scroll event: precise deltas in pixels, as a trackpad sends, or
    /// with lines set, a mouse wheel's notches.
    Scroll {
        x: f64,
        y: f64,
        dx: i32,
        dy: i32,
        lines: bool,
    },
    /// A drag from another app moving over the view.
    DragOver {
        x: f64,
        y: f64,
    },
    DragExit,
    Drop {
        x: f64,
        y: f64,
        payload: DropPayload,
    },
    Action(AppCommand),
    Resize {
        w: f64,
        h: f64,
        live: bool,
    },
    Move {
        x: f64,
        y: f64,
    },
    Cursor(CursorStyle),
    TabPlus,
    /// A double click on the window's nth tab, counting from 1.
    TabDoubleClick(usize),
    /// Where the middle of the window's nth tab is, counting from 1.
    TabCenter(usize),
    /// A press, held, on the nth tab of the tab bar a window's tab is in:
    /// a drag of the tab begins.
    TabPick {
        window: u64,
        n: usize,
    },
    /// The held tab dragged to a point of a window's view.
    TabDrag {
        window: u64,
        x: f64,
        y: f64,
    },
    /// The held tab let go where it was dragged.
    TabDrop,
    CloseButton,
    TabDetach,
    TabMerge,
    Marked(String),
    FocusWindow(bool),
    /// What a change of the window's backing scale does, as moving it to a
    /// display of that scale would.
    BackingScale(f64),
    Confirm(bool),
    Panes,
    FocusPane(PaneId),
    Screen(Option<PaneId>),
    /// A pane's last drawn frame as text, without taking a new snapshot.
    Drawn(Option<PaneId>),
    Selection(Option<PaneId>),
    /// Moves a window's tab into another window's tab bar.
    TabMove {
        window: u64,
        to: u64,
    },
    Screenshot(PathBuf),
    /// The window as the compositor shows it, title bar and layers included.
    WindowImage(PathBuf),
    Stats,
    /// Forgets the frame times stats reports.
    StatsReset,
    Clipboard,
    SetClipboard(String),
    /// What the Dock or Finder sends when files or folders are opened with
    /// the app (application:openURLs:).
    Open(Vec<PathBuf>),
    /// What a click on the Dock icon sends (applicationShouldHandleReopen:).
    Reopen,
    Quit,
}

pub(super) fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                in_token = true;
                match chars.next() {
                    Some('n') => current.push('\n'),
                    Some('t') => current.push('\t'),
                    Some('r') => current.push('\r'),
                    Some('e') => current.push('\u{1b}'),
                    Some(other) => current.push(other),
                    None => return Err("the line ends with a lone backslash".into()),
                }
            }
            c if c.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            c => {
                current.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

fn number(s: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("not a number: {s:?}"))
}

pub(super) fn parse(tokens: &[String]) -> Result<Command, String> {
    let Some((name, args)) = tokens.split_first() else {
        return Err("empty request".into());
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(match (name.as_str(), args.as_slice()) {
        ("state", []) => Command::State,
        ("menu", []) => Command::Menu,
        ("menu-click", [name]) => Command::MenuClick(
            AppCommand::parse(name).ok_or_else(|| format!("unknown command {name:?}"))?,
        ),
        ("key", [spec]) => match modifier_key(spec) {
            Some((code, flags)) => Command::Modifier { code, flags },
            None => Command::Key(parse_key(spec)?),
        },
        ("text", words) if !words.is_empty() => Command::Text(words.join(" ")),
        ("mouse", [kind, x, y, rest @ ..]) if rest.len() <= 3 => {
            let kind = match *kind {
                "down" => MouseKind::Down,
                "up" => MouseKind::Up,
                "drag" => MouseKind::Drag,
                "move" => MouseKind::Move,
                _ => return Err(format!("unknown mouse event {kind:?}")),
            };
            let (rest, clicks) = match rest.split_last() {
                Some((last, before)) if last.starts_with('x') => (
                    before,
                    last[1..]
                        .parse::<u8>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or_else(|| format!("not a click count: {last:?}"))?,
                ),
                _ => (rest, 1),
            };
            let (button, mods) = match rest {
                [one] => match parse_button(one) {
                    Some(b) => (b, None),
                    None => (0, Some(*one)),
                },
                [button, mods] => (
                    parse_button(button).ok_or_else(|| format!("unknown button {button:?}"))?,
                    Some(*mods),
                ),
                [] => (0, None),
                _ => return Err("too many words after the position".into()),
            };
            let flags = mods.map(parse_mods).transpose()?.unwrap_or(0);
            Command::Mouse {
                kind,
                x: number(x)?,
                y: number(y)?,
                button,
                flags,
                clicks,
            }
        }
        ("scroll", [x, y, dx, dy, rest @ ..]) if matches!(rest, [] | ["lines"]) => {
            let delta = |s: &str| {
                s.parse::<i32>()
                    .map_err(|_| format!("not a whole number: {s:?}"))
            };
            Command::Scroll {
                x: number(x)?,
                y: number(y)?,
                dx: delta(dx)?,
                dy: delta(dy)?,
                lines: !rest.is_empty(),
            }
        }
        ("drag-over", [x, y]) => Command::DragOver {
            x: number(x)?,
            y: number(y)?,
        },
        ("drag-exit", []) => Command::DragExit,
        ("drop", [x, y, "path", paths @ ..]) if !paths.is_empty() => Command::Drop {
            x: number(x)?,
            y: number(y)?,
            payload: DropPayload::Paths(paths.iter().map(PathBuf::from).collect()),
        },
        ("drop", [x, y, "text", words @ ..]) if !words.is_empty() => Command::Drop {
            x: number(x)?,
            y: number(y)?,
            payload: DropPayload::Text(words.join(" ")),
        },
        ("action", [name]) => Command::Action(
            AppCommand::parse(name).ok_or_else(|| format!("unknown action {name:?}"))?,
        ),
        ("resize", [w, h, rest @ ..]) if matches!(rest, [] | ["live"]) => {
            let (w, h) = (number(w)?, number(h)?);
            if w < 1.0 || h < 1.0 {
                return Err("the size must be positive".into());
            }
            Command::Resize {
                w,
                h,
                live: !rest.is_empty(),
            }
        }
        ("move", [x, y]) => Command::Move {
            x: number(x)?,
            y: number(y)?,
        },
        ("cursor", [style]) => Command::Cursor(
            CursorStyle::ALL
                .into_iter()
                .find(|s| s.name() == *style)
                .ok_or_else(|| format!("unknown cursor {style:?}"))?,
        ),
        ("tab-plus", []) => Command::TabPlus,
        ("tab-double-click", [n]) => Command::TabDoubleClick(
            n.parse()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| format!("not a tab number: {n:?}"))?,
        ),
        ("tab-center", [n]) => Command::TabCenter(
            n.parse()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| format!("not a tab number: {n:?}"))?,
        ),
        ("tab-pick", [window, n]) => Command::TabPick {
            window: window_id(window)?,
            n: n.parse()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| format!("not a tab number: {n:?}"))?,
        },
        ("tab-drag", [window, x, y]) => Command::TabDrag {
            window: window_id(window)?,
            x: number(x)?,
            y: number(y)?,
        },
        ("tab-drop", []) => Command::TabDrop,
        ("close-button", []) => Command::CloseButton,
        ("tab-detach", []) => Command::TabDetach,
        ("tab-merge", []) => Command::TabMerge,
        ("marked", words) if !words.is_empty() => Command::Marked(words.join(" ")),
        ("focus-window", ["on"]) => Command::FocusWindow(true),
        ("focus-window", ["off"]) => Command::FocusWindow(false),
        ("backing-scale", [scale]) => Command::BackingScale(
            scale
                .parse()
                .ok()
                .filter(|s: &f64| (1.0..=4.0).contains(s))
                .ok_or_else(|| format!("not a backing scale from 1 to 4: {scale:?}"))?,
        ),
        ("confirm", ["yes"]) => Command::Confirm(true),
        ("confirm", ["no"]) => Command::Confirm(false),
        ("panes", []) => Command::Panes,
        ("focus-pane", [pane]) => Command::FocusPane(
            pane.parse()
                .map_err(|_| format!("not a pane id: {pane:?}"))?,
        ),
        ("screen", []) => Command::Screen(None),
        ("screen", [pane]) => Command::Screen(Some(
            pane.parse()
                .map_err(|_| format!("not a pane id: {pane:?}"))?,
        )),
        ("drawn", []) => Command::Drawn(None),
        ("drawn", [pane]) => Command::Drawn(Some(pane_id(pane)?)),
        ("selection", []) => Command::Selection(None),
        ("selection", [pane]) => Command::Selection(Some(pane_id(pane)?)),
        ("tab-move", [window, to]) => Command::TabMove {
            window: window_id(window)?,
            to: window_id(to)?,
        },
        ("screenshot", [path]) => Command::Screenshot(PathBuf::from(path)),
        ("window-image", [path]) => Command::WindowImage(PathBuf::from(path)),
        ("stats", []) => Command::Stats,
        ("stats-reset", []) => Command::StatsReset,
        ("clipboard", []) => Command::Clipboard,
        ("clipboard-set", words) if !words.is_empty() => Command::SetClipboard(words.join(" ")),
        ("open", paths) if !paths.is_empty() => {
            Command::Open(paths.iter().map(PathBuf::from).collect())
        }
        ("reopen", []) => Command::Reopen,
        ("quit", []) => Command::Quit,
        _ => {
            return Err(format!(
                "unknown or malformed request: {}",
                tokens.join(" ")
            ))
        }
    })
}

fn window_id(s: &str) -> Result<u64, String> {
    s.parse().map_err(|_| format!("not a window id: {s:?}"))
}

fn pane_id(s: &str) -> Result<PaneId, String> {
    s.parse().map_err(|_| format!("not a pane id: {s:?}"))
}

fn parse_button(s: &str) -> Option<u8> {
    match s {
        "left" => Some(0),
        "right" => Some(1),
        "middle" => Some(2),
        _ => s.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::flag;
    use tern_config::Action;

    fn tokens(line: &str) -> Vec<String> {
        tokenize(line).expect("tokenizes")
    }

    #[test]
    fn tokenizer_splits_and_unescapes() {
        assert_eq!(tokens("  state  "), ["state"]);
        assert_eq!(tokens("text hello\\ world"), ["text", "hello world"]);
        assert_eq!(
            tokens("text a\\nb \\t \\\\ \\e"),
            ["text", "a\nb", "\t", "\\", "\u{1b}"]
        );
        assert_eq!(tokens("text \\ "), ["text", " "]);
        assert!(tokens("").is_empty());
        assert!(tokenize("text \\").is_err());
    }

    fn command(line: &str) -> Result<Command, String> {
        parse(&tokenize(line)?)
    }

    #[test]
    fn requests_parse() {
        assert_eq!(command("state"), Ok(Command::State));
        assert_eq!(
            command("menu-click new_tab"),
            Ok(Command::MenuClick(AppCommand::Do(Action::NewTab)))
        );
        assert_eq!(command("text a  b"), Ok(Command::Text("a b".into())));
        assert_eq!(
            command("mouse down 10 20.5"),
            Ok(Command::Mouse {
                kind: MouseKind::Down,
                x: 10.0,
                y: 20.5,
                button: 0,
                flags: 0,
                clicks: 1
            })
        );
        assert_eq!(
            command("mouse drag 1 2 right shift+cmd"),
            Ok(Command::Mouse {
                kind: MouseKind::Drag,
                x: 1.0,
                y: 2.0,
                button: 1,
                flags: flag::SHIFT | flag::LEFT_SHIFT | flag::COMMAND | flag::LEFT_COMMAND,
                clicks: 1
            })
        );
        assert_eq!(
            command("mouse up 1 2 alt"),
            Ok(Command::Mouse {
                kind: MouseKind::Up,
                x: 1.0,
                y: 2.0,
                button: 0,
                flags: flag::OPTION | flag::LEFT_OPTION,
                clicks: 1
            })
        );
        assert_eq!(
            command("mouse down 1 2 left x2"),
            Ok(Command::Mouse {
                kind: MouseKind::Down,
                x: 1.0,
                y: 2.0,
                button: 0,
                flags: 0,
                clicks: 2
            })
        );
        assert_eq!(
            command("scroll 5 6 0 -30"),
            Ok(Command::Scroll {
                x: 5.0,
                y: 6.0,
                dx: 0,
                dy: -30,
                lines: false
            })
        );
        assert_eq!(
            command("scroll 5 6 0 1 lines"),
            Ok(Command::Scroll {
                x: 5.0,
                y: 6.0,
                dx: 0,
                dy: 1,
                lines: true
            })
        );
        assert_eq!(
            command("drag-over 3 4.5"),
            Ok(Command::DragOver { x: 3.0, y: 4.5 })
        );
        assert_eq!(command("drag-exit"), Ok(Command::DragExit));
        assert_eq!(command("drawn 3"), Ok(Command::Drawn(Some(3))));
        assert_eq!(command("selection"), Ok(Command::Selection(None)));
        assert_eq!(
            command("tab-move 1 2"),
            Ok(Command::TabMove { window: 1, to: 2 })
        );
        assert_eq!(command("stats-reset"), Ok(Command::StatsReset));
        assert_eq!(
            command("drop 1 2 path /a /b\\ c"),
            Ok(Command::Drop {
                x: 1.0,
                y: 2.0,
                payload: DropPayload::Paths(vec!["/a".into(), "/b c".into()])
            })
        );
        assert_eq!(
            command("drop 1 2 text hi there"),
            Ok(Command::Drop {
                x: 1.0,
                y: 2.0,
                payload: DropPayload::Text("hi there".into())
            })
        );
        assert_eq!(
            command("action goto_tab:2"),
            Ok(Command::Action(AppCommand::Do(Action::GotoTab(2))))
        );
        assert_eq!(
            command("action new_tab"),
            Ok(Command::Action(AppCommand::Do(Action::NewTab)))
        );
        assert_eq!(
            command("action text:\\\\x01"),
            Ok(Command::Action(AppCommand::Do(Action::Text(vec![1]))))
        );
        assert_eq!(
            command("action zoom"),
            Ok(Command::Action(AppCommand::Zoom))
        );
        assert_eq!(command("focus-window on"), Ok(Command::FocusWindow(true)));
        assert_eq!(command("focus-window off"), Ok(Command::FocusWindow(false)));
        assert_eq!(command("backing-scale 1"), Ok(Command::BackingScale(1.0)));
        assert!(command("backing-scale 0").is_err());
        assert!(command("backing-scale x").is_err());
        assert_eq!(command("confirm yes"), Ok(Command::Confirm(true)));
        assert_eq!(command("confirm no"), Ok(Command::Confirm(false)));
        assert_eq!(command("panes"), Ok(Command::Panes));
        assert_eq!(command("focus-pane 4"), Ok(Command::FocusPane(4)));
        assert_eq!(command("screen"), Ok(Command::Screen(None)));
        assert_eq!(command("screen 7"), Ok(Command::Screen(Some(7))));
        assert_eq!(
            command("screenshot /tmp/a\\ b.png"),
            Ok(Command::Screenshot("/tmp/a b.png".into()))
        );
        assert_eq!(
            command("window-image /tmp/w.png"),
            Ok(Command::WindowImage("/tmp/w.png".into()))
        );
        assert_eq!(command("stats"), Ok(Command::Stats));
        assert_eq!(command("clipboard"), Ok(Command::Clipboard));
        assert_eq!(
            command("clipboard-set a  b"),
            Ok(Command::SetClipboard("a b".into()))
        );
        assert_eq!(
            command("resize 400 300"),
            Ok(Command::Resize {
                w: 400.0,
                h: 300.0,
                live: false
            })
        );
        assert_eq!(
            command("resize 400 300 live"),
            Ok(Command::Resize {
                w: 400.0,
                h: 300.0,
                live: true
            })
        );
        assert_eq!(
            command("move -10 20.5"),
            Ok(Command::Move { x: -10.0, y: 20.5 })
        );
        assert_eq!(
            command("cursor row-resize"),
            Ok(Command::Cursor(CursorStyle::RowResize))
        );
        assert_eq!(
            command("key ralt"),
            Ok(Command::Modifier {
                code: 0x3D,
                flags: flag::OPTION | flag::RIGHT_OPTION
            })
        );
        assert_eq!(command("tab-plus"), Ok(Command::TabPlus));
        assert_eq!(
            command("tab-double-click 2"),
            Ok(Command::TabDoubleClick(2))
        );
        assert_eq!(command("tab-center 3"), Ok(Command::TabCenter(3)));
        assert_eq!(
            command("tab-pick 3 2"),
            Ok(Command::TabPick { window: 3, n: 2 })
        );
        assert_eq!(
            command("tab-drag 4 10.5 -3"),
            Ok(Command::TabDrag {
                window: 4,
                x: 10.5,
                y: -3.0
            })
        );
        assert_eq!(command("tab-drop"), Ok(Command::TabDrop));
        assert_eq!(command("close-button"), Ok(Command::CloseButton));
        assert_eq!(command("tab-detach"), Ok(Command::TabDetach));
        assert_eq!(command("marked か"), Ok(Command::Marked("か".into())));
        assert_eq!(command("quit"), Ok(Command::Quit));
        for bad in [
            "",
            "bogus",
            "state x",
            "text",
            "mouse fly 1 2",
            "mouse down x 2",
            "mouse down 1 2 left ctrl extra",
            "mouse down 1 2 hyper",
            "mouse down 1 2 x0",
            "mouse down 1 2 left ctrl x2 more",
            "scroll 1 2 3",
            "scroll 1 2 3 4 notches",
            "drag-over 1",
            "drawn x",
            "selection 1 2",
            "tab-move",
            "tab-move 1",
            "tab-move x 2",
            "tab-double-click",
            "tab-double-click 0",
            "tab-double-click first",
            "tab-center",
            "tab-center 0",
            "tab-pick",
            "tab-pick 1",
            "tab-pick 1 0",
            "tab-pick x 1",
            "tab-drag 1 2",
            "tab-drag x 1 2",
            "tab-drag 1 x 2",
            "tab-drop 1",
            "scroll 1 2 1.5 0",
            "drop 1 2 path",
            "drop 1 2 file /a",
            "action nope",
            "action goto_tab",
            "action goto_tab 2",
            "focus-window maybe",
            "confirm",
            "confirm maybe",
            "screen x",
            "focus-pane",
            "focus-pane x",
            "screenshot",
            "window-image",
            "window-image a b",
            "clipboard-set",
            "resize 0 10",
            "resize 10 10 slowly",
            "cursor wait",
            "key",
            "key ctrl+",
        ] {
            assert!(command(bad).is_err(), "{bad:?} should not parse");
        }
    }
}
