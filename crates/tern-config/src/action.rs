//! What a key binding or a menu item asks the app to do, and how actions are
//! spelled in the config file.

use std::fmt::{self, Write as _};
use std::str::FromStr;

use crate::ParseError;

/// Declares a small enum whose variants have config names, with the
/// conversions both ways.
macro_rules! named {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal,)+ }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            const ALL: &'static [$name] = &[$($name::$variant,)+];

            fn name(self) -> &'static str {
                match self {
                    $($name::$variant => $text,)+
                }
            }

            fn parse(text: &str) -> Result<$name, String> {
                $name::ALL.iter().copied().find(|v| v.name() == text).ok_or_else(|| {
                    let names: Vec<&str> = $name::ALL.iter().map(|v| v.name()).collect();
                    format!("expected one of {}, got \"{text}\"", names.join(", "))
                })
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(self.name())
            }
        }
    };
}

named! {
    /// A side of the focused pane: where a split puts the new pane, or which
    /// way a divider moves.
    Direction {
        Right = "right",
        Down = "down",
        Left = "left",
        Up = "up",
    }
}

named! {
    /// The pane focus moves to.
    FocusTarget {
        Left = "left",
        Right = "right",
        Up = "up",
        Down = "down",
        /// The next pane in the tab's order, wrapping around.
        Next = "next",
        /// The previous pane in the tab's order, wrapping around.
        Previous = "previous",
    }
}

named! {
    /// Where the viewport of the focused pane moves.
    ScrollTarget {
        PageUp = "page_up",
        PageDown = "page_down",
        /// The oldest line of the scrollback.
        Top = "top",
        /// The live screen.
        Bottom = "bottom",
        LineUp = "line_up",
        LineDown = "line_down",
    }
}

named! {
    /// A change to the font size of every pane.
    FontSizeChange {
        Increase = "increase",
        Decrease = "decrease",
        /// Back to the configured size.
        Reset = "reset",
    }
}

named! {
    /// Which OSC 133 semantic prompt to jump to.
    PromptDirection {
        Previous = "previous",
        Next = "next",
    }
}

/// Something a key binding or a menu item asks the app to do.
///
/// The config spelling is snake_case, with a colon before the argument:
/// `new_tab`, `split:right`, `resize_split:up,10`, `text:\x01`. [FromStr] and
/// [Display](fmt::Display) convert both ways.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    NewWindow,
    NewTab,
    /// Close the focused pane; its tab closes with its last pane.
    ClosePane,
    CloseTab,
    CloseWindow,
    /// Split the focused pane, putting the new pane on this side.
    Split(Direction),
    FocusSplit(FocusTarget),
    /// Move the divider nearest the focused pane in this direction by this
    /// many points.
    ResizeSplit(Direction, u16),
    /// Give every pane in the tab its equal share.
    EqualizeSplits,
    /// Show the focused pane alone in its tab, or undo that.
    ToggleZoom,
    /// Go to tab 1 to 8 of the window; 9 means the last tab.
    GotoTab(u8),
    NextTab,
    PreviousTab,
    /// Move the focused tab this many places to the right (negative: left).
    MoveTab(i8),
    ToggleTabBar,
    MergeAllWindows,
    MoveTabToNewWindow,
    RenameTab,
    Copy,
    Paste,
    SelectAll,
    SelectCommandOutput,
    Find,
    FindNext,
    FindPrevious,
    /// Clear the focused pane's screen and scrollback.
    Clear,
    Scroll(ScrollTarget),
    JumpPrompt(PromptDirection),
    FontSize(FontSizeChange),
    ToggleFullscreen,
    OpenConfig,
    ReloadConfig,
    Quit,
    /// Send these bytes to the focused pane as if they were typed.
    Text(Vec<u8>),
    /// Do nothing, and keep the key from reaching the terminal.
    Ignore,
}

impl FromStr for Action {
    type Err = ParseError;

    fn from_str(text: &str) -> Result<Action, ParseError> {
        parse_action(text).map_err(ParseError)
    }
}

pub(crate) fn parse_action(text: &str) -> Result<Action, String> {
    let (name, arg) = match text.split_once(':') {
        Some((name, arg)) => (name, Some(arg)),
        None => (text, None),
    };
    let plain = match name {
        "new_window" => Some(Action::NewWindow),
        "new_tab" => Some(Action::NewTab),
        "close_pane" => Some(Action::ClosePane),
        "close_tab" => Some(Action::CloseTab),
        "close_window" => Some(Action::CloseWindow),
        "equalize_splits" => Some(Action::EqualizeSplits),
        "toggle_zoom" => Some(Action::ToggleZoom),
        "next_tab" => Some(Action::NextTab),
        "previous_tab" => Some(Action::PreviousTab),
        "toggle_tab_bar" => Some(Action::ToggleTabBar),
        "merge_all_windows" => Some(Action::MergeAllWindows),
        "move_tab_to_new_window" => Some(Action::MoveTabToNewWindow),
        "rename_tab" => Some(Action::RenameTab),
        "copy" => Some(Action::Copy),
        "paste" => Some(Action::Paste),
        "select_all" => Some(Action::SelectAll),
        "select_command_output" => Some(Action::SelectCommandOutput),
        "find" => Some(Action::Find),
        "find_next" => Some(Action::FindNext),
        "find_previous" => Some(Action::FindPrevious),
        "clear" => Some(Action::Clear),
        "toggle_fullscreen" => Some(Action::ToggleFullscreen),
        "open_config" => Some(Action::OpenConfig),
        "reload_config" => Some(Action::ReloadConfig),
        "quit" => Some(Action::Quit),
        "ignore" => Some(Action::Ignore),
        _ => None,
    };
    if let Some(action) = plain {
        return match arg {
            None => Ok(action),
            Some(_) => Err(format!("{name} takes no argument")),
        };
    }

    // The bytes to send are taken exactly as written, spaces included.
    match (name, arg) {
        ("text", Some(arg)) => return Ok(Action::Text(unescape(arg)?)),
        ("esc", Some(arg)) => return Ok(Action::Text(prefixed(b"\x1b", arg)?)),
        ("csi", Some(arg)) => return Ok(Action::Text(prefixed(b"\x1b[", arg)?)),
        _ => {}
    }

    let needs = |usage: &str| format!("{name} needs an argument, as in {usage}");
    let arg = arg.map(str::trim);
    match name {
        "split" => {
            let arg = arg.ok_or_else(|| needs("split:right"))?;
            Ok(Action::Split(Direction::parse(arg)?))
        }
        "focus_split" => {
            let arg = arg.ok_or_else(|| needs("focus_split:next"))?;
            Ok(Action::FocusSplit(FocusTarget::parse(arg)?))
        }
        "resize_split" => {
            let arg = arg.ok_or_else(|| needs("resize_split:up,10"))?;
            let (direction, amount) = arg.split_once(',').ok_or_else(|| {
                format!("expected a direction and a distance in points, as in resize_split:up,10, got \"{arg}\"")
            })?;
            let direction = Direction::parse(direction.trim())?;
            let amount = amount.trim();
            let points = amount
                .parse::<u16>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| {
                    format!(
                        "expected a distance from 1 to {} points, got \"{amount}\"",
                        u16::MAX
                    )
                })?;
            Ok(Action::ResizeSplit(direction, points))
        }
        "goto_tab" => {
            let arg = arg.ok_or_else(|| needs("goto_tab:1"))?;
            let tab = arg.parse::<u8>().ok().filter(|n| (1..=9).contains(n));
            tab.map(Action::GotoTab).ok_or_else(|| {
                format!("expected a tab from 1 to 9 (9 is the last tab), got \"{arg}\"")
            })
        }
        "move_tab" => {
            let arg = arg.ok_or_else(|| needs("move_tab:1"))?;
            let offset = arg.parse::<i8>().ok().filter(|&n| n != 0);
            offset
                .map(Action::MoveTab)
                .ok_or_else(|| format!("expected a nonzero offset from -128 to 127, got \"{arg}\""))
        }
        "scroll" => {
            let arg = arg.ok_or_else(|| needs("scroll:page_up"))?;
            Ok(Action::Scroll(ScrollTarget::parse(arg)?))
        }
        "jump_prompt" => {
            let arg = arg.ok_or_else(|| needs("jump_prompt:previous"))?;
            Ok(Action::JumpPrompt(PromptDirection::parse(arg)?))
        }
        "font_size" => {
            let arg = arg.ok_or_else(|| needs("font_size:increase"))?;
            Ok(Action::FontSize(FontSizeChange::parse(arg)?))
        }
        "text" | "esc" | "csi" => Err(needs(&format!("{name}:abc"))),
        _ => Err(format!("unknown action \"{text}\"")),
    }
}

fn prefixed(prefix: &[u8], text: &str) -> Result<Vec<u8>, String> {
    let mut bytes = prefix.to_vec();
    bytes.extend(unescape(text)?);
    Ok(bytes)
}

/// The bytes a text:, esc: or csi: argument stands for: its UTF-8 with the
/// escapes \x.., \e, \n, \r, \t and \\ replaced.
fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            continue;
        }
        let byte = match chars.next() {
            Some('x') => {
                let high = chars.next().and_then(|c| c.to_digit(16));
                let low = chars.next().and_then(|c| c.to_digit(16));
                match (high, low) {
                    // Two hex digits always fit in a byte.
                    (Some(high), Some(low)) => (high << 4 | low) as u8,
                    _ => return Err("\\x needs two hex digits, as in \\x1b".into()),
                }
            }
            Some('e') => 0x1b,
            Some('n') => b'\n',
            Some('r') => b'\r',
            Some('t') => b'\t',
            Some('\\') => b'\\',
            Some(other) => {
                return Err(format!(
                    "unknown escape \\{other}; the escapes are \\x.., \\e, \\n, \\r, \\t and \\\\"
                ))
            }
            None => return Err("ends with a lone \\".into()),
        };
        out.push(byte);
    }
    Ok(out)
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self {
            Action::NewWindow => "new_window",
            Action::NewTab => "new_tab",
            Action::ClosePane => "close_pane",
            Action::CloseTab => "close_tab",
            Action::CloseWindow => "close_window",
            Action::EqualizeSplits => "equalize_splits",
            Action::ToggleZoom => "toggle_zoom",
            Action::NextTab => "next_tab",
            Action::PreviousTab => "previous_tab",
            Action::ToggleTabBar => "toggle_tab_bar",
            Action::MergeAllWindows => "merge_all_windows",
            Action::MoveTabToNewWindow => "move_tab_to_new_window",
            Action::RenameTab => "rename_tab",
            Action::Copy => "copy",
            Action::Paste => "paste",
            Action::SelectAll => "select_all",
            Action::SelectCommandOutput => "select_command_output",
            Action::Find => "find",
            Action::FindNext => "find_next",
            Action::FindPrevious => "find_previous",
            Action::Clear => "clear",
            Action::ToggleFullscreen => "toggle_fullscreen",
            Action::OpenConfig => "open_config",
            Action::ReloadConfig => "reload_config",
            Action::Quit => "quit",
            Action::Ignore => "ignore",
            Action::Split(direction) => return write!(f, "split:{direction}"),
            Action::FocusSplit(target) => return write!(f, "focus_split:{target}"),
            Action::ResizeSplit(direction, points) => {
                return write!(f, "resize_split:{direction},{points}")
            }
            Action::GotoTab(tab) => return write!(f, "goto_tab:{tab}"),
            Action::MoveTab(offset) => return write!(f, "move_tab:{offset}"),
            Action::Scroll(target) => return write!(f, "scroll:{target}"),
            Action::JumpPrompt(dir) => return write!(f, "jump_prompt:{dir}"),
            Action::FontSize(change) => return write!(f, "font_size:{change}"),
            Action::Text(bytes) => return write_text(f, bytes),
        };
        f.write_str(name)
    }
}

/// Writes bytes in the spelling a person would choose: csi: or esc: when they
/// start with an escape sequence, text: otherwise.
fn write_text(f: &mut fmt::Formatter, bytes: &[u8]) -> fmt::Result {
    let (prefix, rest) = if let Some(rest) = bytes.strip_prefix(b"\x1b[") {
        ("csi:", rest)
    } else if let Some(rest) = bytes.strip_prefix(b"\x1b") {
        ("esc:", rest)
    } else {
        ("text:", bytes)
    };
    f.write_str(prefix)?;
    for chunk in rest.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                c if c.is_control() => {
                    for byte in c.encode_utf8(&mut [0; 4]).bytes() {
                        write!(f, "\\x{byte:02x}")?;
                    }
                }
                c => f.write_char(c)?,
            }
        }
        for byte in chunk.invalid() {
            write!(f, "\\x{byte:02x}")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Action {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn error(text: &str) -> String {
        match text.parse::<Action>() {
            Ok(action) => panic!("{text} parsed as {action:?}"),
            Err(e) => e.to_string(),
        }
    }

    /// One of every kind of action, with arguments at their edges.
    fn samples() -> Vec<Action> {
        let mut all = vec![
            Action::NewWindow,
            Action::NewTab,
            Action::ClosePane,
            Action::CloseTab,
            Action::CloseWindow,
            Action::EqualizeSplits,
            Action::ToggleZoom,
            Action::GotoTab(1),
            Action::GotoTab(9),
            Action::NextTab,
            Action::PreviousTab,
            Action::ToggleTabBar,
            Action::MergeAllWindows,
            Action::MoveTabToNewWindow,
            Action::RenameTab,
            Action::MoveTab(1),
            Action::MoveTab(-1),
            Action::MoveTab(i8::MIN),
            Action::MoveTab(i8::MAX),
            Action::Copy,
            Action::Paste,
            Action::SelectAll,
            Action::SelectCommandOutput,
            Action::Find,
            Action::FindNext,
            Action::FindPrevious,
            Action::Clear,
            Action::ToggleFullscreen,
            Action::OpenConfig,
            Action::ReloadConfig,
            Action::Quit,
            Action::Ignore,
            Action::ResizeSplit(Direction::Up, 1),
            Action::ResizeSplit(Direction::Left, u16::MAX),
            Action::Text(vec![]),
            Action::Text(vec![0x01]),
            Action::Text(b"\x1b".to_vec()),
            Action::Text(b"\x1b[".to_vec()),
            Action::Text(b"\x1b[1;5A".to_vec()),
            Action::Text(b"\x1bb".to_vec()),
            Action::Text(b"\x1b\x1b".to_vec()),
            Action::Text(b"a\\b\n\r\t\x7f \x00".to_vec()),
            Action::Text("caf\u{e9} \u{1f600} \u{85}".as_bytes().to_vec()),
            Action::Text(vec![0xff, b'a', 0xc3]),
            Action::Text(b"key=value:x".to_vec()),
        ];
        all.extend(Direction::ALL.iter().map(|&d| Action::Split(d)));
        all.extend(FocusTarget::ALL.iter().map(|&t| Action::FocusSplit(t)));
        all.extend(ScrollTarget::ALL.iter().map(|&t| Action::Scroll(t)));
        all.extend(PromptDirection::ALL.iter().map(|&d| Action::JumpPrompt(d)));
        all.extend(FontSizeChange::ALL.iter().map(|&c| Action::FontSize(c)));
        all
    }

    #[test]
    fn plain_actions_parse() {
        let cases = [
            ("new_window", Action::NewWindow),
            ("new_tab", Action::NewTab),
            ("close_pane", Action::ClosePane),
            ("close_tab", Action::CloseTab),
            ("close_window", Action::CloseWindow),
            ("equalize_splits", Action::EqualizeSplits),
            ("toggle_zoom", Action::ToggleZoom),
            ("next_tab", Action::NextTab),
            ("previous_tab", Action::PreviousTab),
            ("toggle_tab_bar", Action::ToggleTabBar),
            ("merge_all_windows", Action::MergeAllWindows),
            ("move_tab_to_new_window", Action::MoveTabToNewWindow),
            ("rename_tab", Action::RenameTab),
            ("copy", Action::Copy),
            ("paste", Action::Paste),
            ("select_all", Action::SelectAll),
            ("select_command_output", Action::SelectCommandOutput),
            ("find", Action::Find),
            ("find_next", Action::FindNext),
            ("find_previous", Action::FindPrevious),
            ("clear", Action::Clear),
            ("toggle_fullscreen", Action::ToggleFullscreen),
            ("open_config", Action::OpenConfig),
            ("reload_config", Action::ReloadConfig),
            ("quit", Action::Quit),
            ("ignore", Action::Ignore),
        ];
        for (text, action) in cases {
            assert_eq!(parse(text), action);
            assert_eq!(action.to_string(), text);
        }
    }

    #[test]
    fn actions_with_arguments_parse() {
        assert_eq!(parse("split:right"), Action::Split(Direction::Right));
        assert_eq!(parse("split:down"), Action::Split(Direction::Down));
        assert_eq!(parse("split:left"), Action::Split(Direction::Left));
        assert_eq!(parse("split:up"), Action::Split(Direction::Up));
        assert_eq!(
            parse("focus_split:left"),
            Action::FocusSplit(FocusTarget::Left)
        );
        assert_eq!(
            parse("focus_split:right"),
            Action::FocusSplit(FocusTarget::Right)
        );
        assert_eq!(parse("focus_split:up"), Action::FocusSplit(FocusTarget::Up));
        assert_eq!(
            parse("focus_split:down"),
            Action::FocusSplit(FocusTarget::Down)
        );
        assert_eq!(
            parse("focus_split:next"),
            Action::FocusSplit(FocusTarget::Next)
        );
        assert_eq!(
            parse("focus_split:previous"),
            Action::FocusSplit(FocusTarget::Previous)
        );
        assert_eq!(
            parse("resize_split:up,10"),
            Action::ResizeSplit(Direction::Up, 10)
        );
        assert_eq!(
            parse("resize_split: left , 25 "),
            Action::ResizeSplit(Direction::Left, 25)
        );
        assert_eq!(parse("goto_tab:3"), Action::GotoTab(3));
        assert_eq!(parse("goto_tab:9"), Action::GotoTab(9));
        assert_eq!(parse("move_tab:-2"), Action::MoveTab(-2));
        assert_eq!(parse("move_tab:+1"), Action::MoveTab(1));
        assert_eq!(
            parse("scroll:page_up"),
            Action::Scroll(ScrollTarget::PageUp)
        );
        assert_eq!(
            parse("scroll:page_down"),
            Action::Scroll(ScrollTarget::PageDown)
        );
        assert_eq!(parse("scroll:top"), Action::Scroll(ScrollTarget::Top));
        assert_eq!(parse("scroll:bottom"), Action::Scroll(ScrollTarget::Bottom));
        assert_eq!(
            parse("scroll:line_up"),
            Action::Scroll(ScrollTarget::LineUp)
        );
        assert_eq!(
            parse("scroll:line_down"),
            Action::Scroll(ScrollTarget::LineDown)
        );
        assert_eq!(
            parse("font_size:increase"),
            Action::FontSize(FontSizeChange::Increase)
        );
        assert_eq!(
            parse("font_size:decrease"),
            Action::FontSize(FontSizeChange::Decrease)
        );
        assert_eq!(
            parse("font_size:reset"),
            Action::FontSize(FontSizeChange::Reset)
        );
    }

    #[test]
    fn text_escapes() {
        assert_eq!(parse(r"text:\x01"), Action::Text(vec![0x01]));
        assert_eq!(parse(r"text:\x1B\x1b"), Action::Text(vec![0x1b, 0x1b]));
        assert_eq!(parse(r"text:\e"), Action::Text(vec![0x1b]));
        assert_eq!(
            parse(r"text:a\nb\rc\td\\e"),
            Action::Text(b"a\nb\rc\td\\e".to_vec())
        );
        assert_eq!(
            parse("text:caf\u{e9}"),
            Action::Text("caf\u{e9}".as_bytes().to_vec())
        );
        assert_eq!(parse("text: spaced "), Action::Text(b" spaced ".to_vec()));
        assert_eq!(parse("text:a:b=c"), Action::Text(b"a:b=c".to_vec()));
        assert_eq!(parse("text:"), Action::Text(vec![]));
        assert_eq!(parse("esc:b"), Action::Text(b"\x1bb".to_vec()));
        assert_eq!(parse("esc:"), Action::Text(b"\x1b".to_vec()));
        assert_eq!(parse(r"esc:\x7f"), Action::Text(b"\x1b\x7f".to_vec()));
        assert_eq!(parse("csi:1;5D"), Action::Text(b"\x1b[1;5D".to_vec()));

        assert!(error(r"text:\q").contains("unknown escape \\q"));
        assert!(error(r"text:abc\").contains("lone"));
        assert!(error(r"text:\x1").contains("two hex digits"));
        assert!(error(r"text:\xg0").contains("two hex digits"));
        assert!(error(r"text:\x+f").contains("two hex digits"));
        assert!(error(r"esc:\z").contains("unknown escape"));
    }

    #[test]
    fn bad_actions_are_rejected() {
        assert_eq!(error("frobnicate"), "unknown action \"frobnicate\"");
        assert_eq!(error("NEW_TAB"), "unknown action \"NEW_TAB\"");
        assert_eq!(error(""), "unknown action \"\"");
        assert_eq!(error("unbind"), "unknown action \"unbind\"");
        assert_eq!(error("new_tab:2"), "new_tab takes no argument");
        assert_eq!(error("quit:"), "quit takes no argument");
        assert!(error("split").contains("needs an argument"));
        assert!(error("split:sideways").contains("expected one of right, down, left, up"));
        assert!(error("focus_split").contains("needs an argument"));
        assert!(error("focus_split:middle").contains("next, previous"));
        assert!(error("resize_split").contains("needs an argument"));
        assert!(error("resize_split:up").contains("direction and a distance"));
        assert!(error("resize_split:up,0").contains("from 1 to 65535"));
        assert!(error("resize_split:up,70000").contains("from 1 to 65535"));
        assert!(error("resize_split:up,-5").contains("from 1 to 65535"));
        assert!(error("resize_split:next,5").contains("expected one of"));
        assert!(error("goto_tab").contains("needs an argument"));
        assert!(error("goto_tab:0").contains("from 1 to 9"));
        assert!(error("goto_tab:10").contains("from 1 to 9"));
        assert!(error("goto_tab:last").contains("from 1 to 9"));
        assert!(error("move_tab").contains("needs an argument"));
        assert!(error("move_tab:0").contains("nonzero"));
        assert!(error("move_tab:200").contains("nonzero"));
        assert!(error("scroll").contains("needs an argument"));
        assert!(error("scroll:sideways").contains("page_up"));
        assert!(error("font_size").contains("needs an argument"));
        assert!(error("font_size:huge").contains("increase"));
        assert!(error("text").contains("needs an argument"));
        assert!(error("esc").contains("needs an argument"));
        assert!(error("csi").contains("needs an argument"));
    }

    #[test]
    fn every_action_round_trips_through_its_spelling() {
        for action in samples() {
            let text = action.to_string();
            assert_eq!(parse(&text), action, "{text}");
        }
    }

    #[test]
    fn canonical_spellings_format_back_unchanged() {
        let canonical = [
            "split:down",
            "focus_split:previous",
            "resize_split:right,10",
            "goto_tab:9",
            "move_tab:-1",
            "scroll:line_down",
            "font_size:reset",
            r"text:\x01",
            r"text:\x15",
            r"text:a\\b\n\r\t",
            "text:caf\u{e9}",
            r"text:\xff",
            "esc:b",
            "esc:",
            "csi:A",
            "csi:",
            r"esc:\x1b",
        ];
        for text in canonical {
            assert_eq!(parse(text).to_string(), text);
        }
        // Equivalent spellings settle on one.
        assert_eq!(parse(r"text:\e[A").to_string(), "csi:A");
        assert_eq!(parse(r"text:\x1bf").to_string(), "esc:f");
        assert_eq!(parse(r"esc:[A").to_string(), "csi:A");
    }
    /// Every string of up to two bytes, alone and after ESC and ESC [, so
    /// every escape, every invalid UTF-8 start and every prefix choice is
    /// covered.
    #[test]
    fn every_short_byte_string_round_trips() {
        let mut strings = vec![vec![]];
        strings.extend((0..=255u8).map(|a| vec![a]));
        for a in 0..=255u8 {
            strings.extend((0..=255u8).map(|b| vec![a, b]));
        }
        for bytes in strings {
            for prefix in [&b""[..], b"\x1b", b"\x1b["] {
                let action = Action::Text([prefix, &bytes].concat());
                let text = action.to_string();
                assert_eq!(text.parse::<Action>().ok(), Some(action), "{text:?}");
            }
        }
    }
}
