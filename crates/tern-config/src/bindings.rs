//! Key bindings: triggers, the binding table and its defaults.

use std::fmt;
use std::str::FromStr;

use tern_types::{Key, KeypadKey, Mods};

use crate::action::{
    parse_action, Action, Direction, FocusTarget, FontSizeChange, PromptDirection, ScrollTarget,
};
use crate::ParseError;

/// A key and the modifiers that must be held with it, written like
/// `cmd+shift+d`. The key is matched by its base character, so `cmd+shift+d`
/// is the D key with Command and Shift held, and `cmd+shift+]` is the ] key
/// with Command and Shift held, whatever character Shift turns it into.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Trigger {
    key: Key,
    mods: Mods,
}

impl Trigger {
    /// A trigger for this key and these modifiers; lock modifiers are
    /// dropped and letters lowercased. None for keys that cannot be bound:
    /// control characters, spaces other than the space bar, F0 and function
    /// keys past F24, keypad digits past 9, and Caps Lock, Menu and the
    /// modifier keys on their own.
    pub fn new(key: Key, mods: Mods) -> Option<Trigger> {
        let key = match key {
            Key::Char(c) if c.is_control() => return None,
            // The parser trims them, so a trigger holding one could not be
            // written down and read back.
            Key::Char(c) if c.is_whitespace() && c != ' ' => return None,
            Key::Char(c) => Key::Char(lowercase(c)),
            Key::F(n) if !(1..=24).contains(&n) => return None,
            Key::Keypad(KeypadKey::Digit(d)) if d > 9 => return None,
            Key::CapsLock
            | Key::Menu
            | Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight => return None,
            key => key,
        };
        Some(Trigger {
            key,
            mods: mods.held(),
        })
    }

    pub fn key(self) -> Key {
        self.key
    }

    /// Shift, Alt, Ctrl and Super only.
    pub fn mods(self) -> Mods {
        self.mods
    }
}

/// Keys written by name. Characters that have names are still written as
/// themselves, except those in SPELLED_OUT.
pub(crate) const KEY_NAMES: &[(&str, Key)] = &[
    ("enter", Key::Enter),
    ("tab", Key::Tab),
    ("backspace", Key::Backspace),
    ("escape", Key::Escape),
    ("space", Key::Char(' ')),
    ("up", Key::Up),
    ("down", Key::Down),
    ("left", Key::Left),
    ("right", Key::Right),
    ("home", Key::Home),
    ("end", Key::End),
    ("page_up", Key::PageUp),
    ("page_down", Key::PageDown),
    ("insert", Key::Insert),
    ("delete", Key::Delete),
    ("plus", Key::Char('+')),
    ("minus", Key::Char('-')),
    ("equal", Key::Char('=')),
    ("comma", Key::Char(',')),
    ("period", Key::Char('.')),
    ("slash", Key::Char('/')),
    ("backslash", Key::Char('\\')),
    ("semicolon", Key::Char(';')),
    ("apostrophe", Key::Char('\'')),
    ("grave", Key::Char('\u{60}')),
    ("left_bracket", Key::Char('[')),
    ("right_bracket", Key::Char(']')),
    ("kp_decimal", Key::Keypad(KeypadKey::Decimal)),
    ("kp_divide", Key::Keypad(KeypadKey::Divide)),
    ("kp_multiply", Key::Keypad(KeypadKey::Multiply)),
    ("kp_subtract", Key::Keypad(KeypadKey::Subtract)),
    ("kp_add", Key::Keypad(KeypadKey::Add)),
    ("kp_enter", Key::Keypad(KeypadKey::Enter)),
    ("kp_equal", Key::Keypad(KeypadKey::Equal)),
];

/// Characters written by name: + and = are part of the binding syntax, and
/// the others are easy to misread on their own.
const SPELLED_OUT: &[char] = &['+', '=', '-', ',', ' '];

/// The modifiers in the order they are written.
const MODIFIERS: [(Mods, &str); 4] = [
    (Mods::SUPER, "cmd"),
    (Mods::CTRL, "ctrl"),
    (Mods::ALT, "alt"),
    (Mods::SHIFT, "shift"),
];

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (modifier, name) in MODIFIERS {
            if self.mods.contains(modifier) {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            Key::Char(c) if !SPELLED_OUT.contains(&c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "f{n}"),
            Key::Keypad(KeypadKey::Digit(d)) => write!(f, "kp_{d}"),
            key => match KEY_NAMES.iter().find(|(_, k)| *k == key) {
                Some((name, _)) => f.write_str(name),
                // Trigger::new admits no key without a name; this keeps
                // Display total all the same.
                None => write!(f, "{key:?}"),
            },
        }
    }
}

impl FromStr for Trigger {
    type Err = ParseError;

    fn from_str(text: &str) -> Result<Trigger, ParseError> {
        parse_trigger(text).map_err(ParseError)
    }
}

fn parse_trigger(text: &str) -> Result<Trigger, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("missing trigger".into());
    }
    // A trailing + is the plus key itself, as in "cmd++" or "cmd + +".
    let (modifiers, key) = match text.strip_suffix('+').map(str::trim_end) {
        Some("") => ("", "+"),
        Some(rest) => match rest.strip_suffix('+') {
            Some(modifiers) => (modifiers, "+"),
            None => return Err(format!("\"{text}\" has no key after its last +")),
        },
        None => text.rsplit_once('+').unwrap_or(("", text)),
    };
    let mut mods = Mods::NONE;
    if !modifiers.is_empty() {
        for name in modifiers.split('+') {
            mods = mods | parse_modifier(name.trim())?;
        }
    }
    let key = parse_key(key.trim())?;
    Trigger::new(key, mods).ok_or_else(|| format!("\"{text}\" cannot be bound"))
}

fn parse_modifier(name: &str) -> Result<Mods, String> {
    match name.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "super" => Ok(Mods::SUPER),
        "ctrl" | "control" => Ok(Mods::CTRL),
        "alt" | "opt" | "option" => Ok(Mods::ALT),
        "shift" => Ok(Mods::SHIFT),
        _ => Err(format!(
            "unknown modifier \"{name}\"; the modifiers are cmd, ctrl, alt and shift"
        )),
    }
}

fn parse_key(name: &str) -> Result<Key, String> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(Key::Char(c));
    }
    let lower = name.to_ascii_lowercase();
    if let Some(&(_, key)) = KEY_NAMES.iter().find(|(n, _)| *n == lower) {
        return Ok(key);
    }
    let number = |prefix: &str| {
        lower
            .strip_prefix(prefix)
            .and_then(|n| n.parse::<u8>().ok())
    };
    if let Some(n) = number("f").filter(|n| (1..=24).contains(n)) {
        return Ok(Key::F(n));
    }
    if let Some(d) = number("kp_").filter(|&d| d <= 9) {
        return Ok(Key::Keypad(KeypadKey::Digit(d)));
    }
    if name.is_empty() {
        Err("missing key".into())
    } else {
        Err(format!("unknown key \"{name}\""))
    }
}

/// The lowercase form of a letter, when it is a single character.
fn lowercase(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// The key binding table: which action each trigger performs.
///
/// Bindings are kept in the order they were made (the defaults first, then
/// the config file's, then the command line's); binding a trigger again
/// replaces its action in place. [Bindings::default] holds the default
/// bindings and [Bindings::empty] none.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bindings {
    entries: Vec<(Trigger, Action)>,
}

impl Bindings {
    pub fn empty() -> Bindings {
        Bindings {
            entries: Vec::new(),
        }
    }

    /// The action bound to this key and these modifiers. Lock modifiers are
    /// ignored; the others must match exactly. The key must be given the
    /// way tern_types::Key describes it, with its base character. An
    /// [Action::Ignore] result means the key should be swallowed.
    pub fn lookup(&self, key: Key, mods: Mods) -> Option<&Action> {
        let mods = mods.held();
        self.entries
            .iter()
            .find(|(trigger, _)| trigger.key == key && trigger.mods == mods)
            .map(|(_, action)| action)
    }

    /// The first trigger bound to this action, for showing as a menu item's
    /// shortcut.
    pub fn shortcut_for(&self, action: &Action) -> Option<(Key, Mods)> {
        self.entries
            .iter()
            .find(|(_, bound)| bound == action)
            .map(|(trigger, _)| (trigger.key, trigger.mods))
    }

    /// Every binding, in the order they were made.
    pub fn iter(&self) -> impl Iterator<Item = (Trigger, &Action)> + '_ {
        self.entries
            .iter()
            .map(|(trigger, action)| (*trigger, action))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Binds the trigger, replacing what it was bound to before.
    pub fn bind(&mut self, trigger: Trigger, action: Action) {
        match self.entries.iter_mut().find(|(t, _)| *t == trigger) {
            Some(entry) => entry.1 = action,
            None => self.entries.push((trigger, action)),
        }
    }

    /// Removes the trigger's binding, so the key reaches the terminal.
    /// Returns the action it was bound to.
    pub fn unbind(&mut self, trigger: Trigger) -> Option<Action> {
        let index = self.entries.iter().position(|(t, _)| *t == trigger)?;
        Some(self.entries.remove(index).1)
    }

    /// Applies one keybind value from the config: "clear",
    /// "<trigger>=unbind" or "<trigger>=<action>".
    pub(crate) fn apply(&mut self, value: &str) -> Result<(), String> {
        if value == "clear" {
            self.entries.clear();
            return Ok(());
        }
        let (trigger, action) = split_binding(value)
            .ok_or_else(|| format!("expected <trigger>=<action> or clear, got \"{value}\""))?;
        let trigger = parse_trigger(trigger)?;
        // Only the start is trimmed: a quoted text: action may end in spaces.
        let action = action.trim_start();
        if action == "unbind" {
            self.unbind(trigger);
        } else {
            self.bind(trigger, parse_action(action)?);
        }
        Ok(())
    }
}

/// Splits "<trigger>=<action>" at the = that ends the trigger. An = at the
/// very start, or right after a + that joins it to modifiers, is the = key,
/// as in "cmd+==ignore"; right after the + key itself, as in "+=new_tab" or
/// "cmd++=new_tab", it ends the trigger.
fn split_binding(value: &str) -> Option<(&str, &str)> {
    let mut at = value.find('=')?;
    let after_joining_plus = value[..at]
        .strip_suffix('+')
        .map(str::trim_end)
        .is_some_and(|rest| !rest.is_empty() && !rest.ends_with('+'));
    if at == 0 || after_joining_plus {
        at += 1 + value[at + 1..].find('=')?;
    }
    Some((&value[..at], &value[at + 1..]))
}

impl Default for Bindings {
    /// The default bindings, following macOS conventions and close to
    /// Ghostty's.
    fn default() -> Bindings {
        const CMD: Mods = Mods::SUPER;
        const CMD_SHIFT: Mods = Mods::SUPER.union(Mods::SHIFT);
        const CMD_ALT: Mods = Mods::SUPER.union(Mods::ALT);
        const CMD_CTRL: Mods = Mods::SUPER.union(Mods::CTRL);
        const CTRL: Mods = Mods::CTRL;
        const CTRL_SHIFT: Mods = Mods::CTRL.union(Mods::SHIFT);
        const SHIFT: Mods = Mods::SHIFT;
        const ALT: Mods = Mods::ALT;
        let bind = |key: Key, mods: Mods, action: Action| (Trigger { key, mods }, action);
        let ch = Key::Char;

        let mut entries = vec![
            bind(ch('n'), CMD, Action::NewWindow),
            bind(ch('t'), CMD, Action::NewTab),
            bind(ch('t'), CMD_SHIFT, Action::ToggleTabBar),
            bind(ch('w'), CMD, Action::ClosePane),
            bind(ch('w'), CMD_SHIFT, Action::CloseTab),
            bind(ch('d'), CMD, Action::Split(Direction::Right)),
            bind(ch('d'), CMD_SHIFT, Action::Split(Direction::Down)),
            bind(ch('['), CMD, Action::FocusSplit(FocusTarget::Previous)),
            bind(ch(']'), CMD, Action::FocusSplit(FocusTarget::Next)),
            bind(Key::Left, CMD_ALT, Action::FocusSplit(FocusTarget::Left)),
            bind(Key::Right, CMD_ALT, Action::FocusSplit(FocusTarget::Right)),
            bind(Key::Up, CMD_ALT, Action::FocusSplit(FocusTarget::Up)),
            bind(Key::Down, CMD_ALT, Action::FocusSplit(FocusTarget::Down)),
            bind(
                Key::Left,
                CMD_CTRL,
                Action::ResizeSplit(Direction::Left, 10),
            ),
            bind(
                Key::Right,
                CMD_CTRL,
                Action::ResizeSplit(Direction::Right, 10),
            ),
            bind(Key::Up, CMD_CTRL, Action::ResizeSplit(Direction::Up, 10)),
            bind(
                Key::Down,
                CMD_CTRL,
                Action::ResizeSplit(Direction::Down, 10),
            ),
            bind(ch('='), CMD_CTRL, Action::EqualizeSplits),
            bind(Key::Enter, CMD_SHIFT, Action::ToggleZoom),
        ];
        entries.extend((1..=9u8).map(|n| bind(ch(char::from(b'0' + n)), CMD, Action::GotoTab(n))));
        entries.extend([
            bind(ch(']'), CMD_SHIFT, Action::NextTab),
            bind(Key::Tab, CTRL, Action::NextTab),
            bind(ch('['), CMD_SHIFT, Action::PreviousTab),
            bind(Key::Tab, CTRL_SHIFT, Action::PreviousTab),
            bind(ch('c'), CMD, Action::Copy),
            bind(ch('v'), CMD, Action::Paste),
            bind(ch('a'), CMD, Action::SelectAll),
            bind(ch('a'), CMD_SHIFT, Action::SelectCommandOutput),
            bind(ch('f'), CMD, Action::Find),
            bind(ch('g'), CMD, Action::FindNext),
            bind(ch('g'), CMD_SHIFT, Action::FindPrevious),
            bind(ch('k'), CMD, Action::Clear),
            bind(Key::Up, CMD, Action::JumpPrompt(PromptDirection::Previous)),
            bind(Key::Down, CMD, Action::JumpPrompt(PromptDirection::Next)),
            bind(Key::PageUp, SHIFT, Action::Scroll(ScrollTarget::PageUp)),
            bind(Key::PageDown, SHIFT, Action::Scroll(ScrollTarget::PageDown)),
            bind(Key::Home, CMD, Action::Scroll(ScrollTarget::Top)),
            bind(Key::End, CMD, Action::Scroll(ScrollTarget::Bottom)),
            bind(ch('='), CMD, Action::FontSize(FontSizeChange::Increase)),
            bind(ch('+'), CMD, Action::FontSize(FontSizeChange::Increase)),
            // On a US keyboard Command-Shift-= is how the + key is typed.
            bind(
                ch('='),
                CMD_SHIFT,
                Action::FontSize(FontSizeChange::Increase),
            ),
            bind(ch('-'), CMD, Action::FontSize(FontSizeChange::Decrease)),
            bind(ch('0'), CMD, Action::FontSize(FontSizeChange::Reset)),
            bind(ch('f'), CMD_CTRL, Action::ToggleFullscreen),
            bind(ch(','), CMD, Action::OpenConfig),
            bind(ch(','), CMD_SHIFT, Action::ReloadConfig),
            bind(ch('q'), CMD, Action::Quit),
            // Readline's start of line, end of line and kill line, and word
            // motion, as macOS text fields do them.
            bind(Key::Left, CMD, Action::Text(vec![0x01])),
            bind(Key::Right, CMD, Action::Text(vec![0x05])),
            bind(Key::Backspace, CMD, Action::Text(vec![0x15])),
            bind(Key::Left, ALT, Action::Text(b"\x1bb".to_vec())),
            bind(Key::Right, ALT, Action::Text(b"\x1bf".to_vec())),
        ]);
        Bindings { entries }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The default bindings as the specification lists them, in order.
    pub(crate) const SPEC_DEFAULTS: &[(&str, &str)] = &[
        ("cmd+n", "new_window"),
        ("cmd+t", "new_tab"),
        ("cmd+shift+t", "toggle_tab_bar"),
        ("cmd+w", "close_pane"),
        ("cmd+shift+w", "close_tab"),
        ("cmd+d", "split:right"),
        ("cmd+shift+d", "split:down"),
        ("cmd+[", "focus_split:previous"),
        ("cmd+]", "focus_split:next"),
        ("cmd+alt+left", "focus_split:left"),
        ("cmd+alt+right", "focus_split:right"),
        ("cmd+alt+up", "focus_split:up"),
        ("cmd+alt+down", "focus_split:down"),
        ("cmd+ctrl+left", "resize_split:left,10"),
        ("cmd+ctrl+right", "resize_split:right,10"),
        ("cmd+ctrl+up", "resize_split:up,10"),
        ("cmd+ctrl+down", "resize_split:down,10"),
        ("cmd+ctrl+equal", "equalize_splits"),
        ("cmd+shift+enter", "toggle_zoom"),
        ("cmd+1", "goto_tab:1"),
        ("cmd+2", "goto_tab:2"),
        ("cmd+3", "goto_tab:3"),
        ("cmd+4", "goto_tab:4"),
        ("cmd+5", "goto_tab:5"),
        ("cmd+6", "goto_tab:6"),
        ("cmd+7", "goto_tab:7"),
        ("cmd+8", "goto_tab:8"),
        ("cmd+9", "goto_tab:9"),
        ("cmd+shift+]", "next_tab"),
        ("ctrl+tab", "next_tab"),
        ("cmd+shift+[", "previous_tab"),
        ("ctrl+shift+tab", "previous_tab"),
        ("cmd+c", "copy"),
        ("cmd+v", "paste"),
        ("cmd+a", "select_all"),
        ("cmd+shift+a", "select_command_output"),
        ("cmd+f", "find"),
        ("cmd+g", "find_next"),
        ("cmd+shift+g", "find_previous"),
        ("cmd+k", "clear"),
        ("cmd+up", "jump_prompt:previous"),
        ("cmd+down", "jump_prompt:next"),
        ("shift+page_up", "scroll:page_up"),
        ("shift+page_down", "scroll:page_down"),
        ("cmd+home", "scroll:top"),
        ("cmd+end", "scroll:bottom"),
        ("cmd+equal", "font_size:increase"),
        ("cmd+plus", "font_size:increase"),
        ("cmd+shift+equal", "font_size:increase"),
        ("cmd+minus", "font_size:decrease"),
        ("cmd+0", "font_size:reset"),
        ("cmd+ctrl+f", "toggle_fullscreen"),
        ("cmd+comma", "open_config"),
        ("cmd+shift+comma", "reload_config"),
        ("cmd+q", "quit"),
        ("cmd+left", r"text:\x01"),
        ("cmd+right", r"text:\x05"),
        ("cmd+backspace", r"text:\x15"),
        ("alt+left", "esc:b"),
        ("alt+right", "esc:f"),
    ];

    fn trigger(text: &str) -> Trigger {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn trigger_error(text: &str) -> String {
        match text.parse::<Trigger>() {
            Ok(t) => panic!("{text} parsed as {t:?}"),
            Err(e) => e.to_string(),
        }
    }

    fn action(text: &str) -> Action {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    const CMD: Mods = Mods::SUPER;

    #[test]
    fn every_default_binding_resolves() {
        let bindings = Bindings::default();
        for &(t, a) in SPEC_DEFAULTS {
            let t = trigger(t);
            assert_eq!(bindings.lookup(t.key(), t.mods()), Some(&action(a)), "{t}");
        }
    }

    #[test]
    fn default_table_is_exactly_the_specified_one() {
        let table: Vec<(String, String)> = Bindings::default()
            .iter()
            .map(|(t, a)| (t.to_string(), a.to_string()))
            .collect();
        let spec: Vec<(String, String)> = SPEC_DEFAULTS
            .iter()
            .map(|&(t, a)| (t.to_owned(), a.to_owned()))
            .collect();
        assert_eq!(table, spec);
    }

    #[test]
    fn lookup_matches_base_characters_and_exact_modifiers() {
        let b = Bindings::default();
        let split_down = Action::Split(Direction::Down);
        // The D key with Command and Shift: the app reports base character 'd'.
        assert_eq!(
            b.lookup(Key::Char('d'), CMD | Mods::SHIFT),
            Some(&split_down)
        );
        // Lock keys do not matter.
        let locked = CMD | Mods::SHIFT | Mods::CAPS_LOCK | Mods::NUM_LOCK;
        assert_eq!(b.lookup(Key::Char('d'), locked), Some(&split_down));
        // Extra or missing modifiers do.
        assert_eq!(
            b.lookup(Key::Char('d'), CMD | Mods::SHIFT | Mods::ALT),
            None
        );
        assert_eq!(b.lookup(Key::Char('d'), Mods::NONE), None);
        assert_eq!(
            b.lookup(Key::Char('d'), CMD),
            Some(&Action::Split(Direction::Right))
        );
        // cmd+shift+] is the ] key with Shift, not the } character.
        assert_eq!(
            b.lookup(Key::Char(']'), CMD | Mods::SHIFT),
            Some(&Action::NextTab)
        );
        assert_eq!(b.lookup(Key::Char('}'), CMD | Mods::SHIFT), None);
        assert_eq!(b.lookup(Key::Tab, Mods::CTRL), Some(&Action::NextTab));
        assert_eq!(
            b.lookup(Key::Tab, Mods::CTRL | Mods::SHIFT),
            Some(&Action::PreviousTab)
        );
        assert_eq!(b.lookup(Key::Char('x'), CMD), None);
    }

    #[test]
    fn shortcut_for_gives_the_first_trigger() {
        let mut b = Bindings::default();
        let next_tab = Action::NextTab;
        assert_eq!(
            b.shortcut_for(&next_tab),
            Some((Key::Char(']'), CMD | Mods::SHIFT))
        );
        let bigger = Action::FontSize(FontSizeChange::Increase);
        assert_eq!(b.shortcut_for(&bigger), Some((Key::Char('='), CMD)));
        assert_eq!(b.shortcut_for(&Action::NewTab), Some((Key::Char('t'), CMD)));
        assert_eq!(b.shortcut_for(&Action::CloseWindow), None);
        b.unbind(trigger("cmd+shift+]"));
        assert_eq!(b.shortcut_for(&next_tab), Some((Key::Tab, Mods::CTRL)));
        b.apply("cmd+shift+n=new_tab").unwrap();
        b.apply("cmd+t=unbind").unwrap();
        assert_eq!(
            b.shortcut_for(&Action::NewTab),
            Some((Key::Char('n'), CMD | Mods::SHIFT))
        );
    }

    #[test]
    fn trigger_parsing() {
        let t = trigger("cmd+shift+d");
        assert_eq!((t.key(), t.mods()), (Key::Char('d'), CMD | Mods::SHIFT));
        // Modifier aliases, any order, any case, spaces around +.
        for text in [
            "super+shift+d",
            "command+shift+d",
            "shift+cmd+d",
            "CMD+Shift+D",
            "cmd + shift + d",
        ] {
            assert_eq!(trigger(text), t, "{text}");
        }
        assert_eq!(trigger("control+x"), trigger("ctrl+x"));
        assert_eq!(trigger("opt+x"), trigger("alt+x"));
        assert_eq!(trigger("option+x"), trigger("alt+x"));
        assert_eq!(trigger("cmd+cmd+x"), trigger("cmd+x"));
        assert_eq!(trigger("x").mods(), Mods::NONE);

        // Key names.
        let named = [
            ("enter", Key::Enter),
            ("tab", Key::Tab),
            ("backspace", Key::Backspace),
            ("escape", Key::Escape),
            ("space", Key::Char(' ')),
            ("up", Key::Up),
            ("down", Key::Down),
            ("left", Key::Left),
            ("right", Key::Right),
            ("home", Key::Home),
            ("end", Key::End),
            ("page_up", Key::PageUp),
            ("page_down", Key::PageDown),
            ("insert", Key::Insert),
            ("delete", Key::Delete),
            ("f1", Key::F(1)),
            ("F12", Key::F(12)),
            ("f24", Key::F(24)),
            ("plus", Key::Char('+')),
            ("minus", Key::Char('-')),
            ("equal", Key::Char('=')),
            ("comma", Key::Char(',')),
            ("period", Key::Char('.')),
            ("slash", Key::Char('/')),
            ("backslash", Key::Char('\\')),
            ("semicolon", Key::Char(';')),
            ("apostrophe", Key::Char('\'')),
            ("grave", Key::Char('\u{60}')),
            ("left_bracket", Key::Char('[')),
            ("right_bracket", Key::Char(']')),
            ("kp_0", Key::Keypad(KeypadKey::Digit(0))),
            ("kp_9", Key::Keypad(KeypadKey::Digit(9))),
            ("kp_enter", Key::Keypad(KeypadKey::Enter)),
            ("kp_decimal", Key::Keypad(KeypadKey::Decimal)),
            ("kp_divide", Key::Keypad(KeypadKey::Divide)),
            ("kp_multiply", Key::Keypad(KeypadKey::Multiply)),
            ("kp_subtract", Key::Keypad(KeypadKey::Subtract)),
            ("kp_add", Key::Keypad(KeypadKey::Add)),
            ("kp_equal", Key::Keypad(KeypadKey::Equal)),
            ("PAGE_UP", Key::PageUp),
        ];
        for (name, key) in named {
            assert_eq!(trigger(&format!("cmd+{name}")).key(), key, "{name}");
        }

        // Single characters, letters folded to lowercase.
        assert_eq!(trigger("cmd+D"), trigger("cmd+d"));
        assert_eq!(trigger("cmd+[").key(), Key::Char('['));
        assert_eq!(trigger("cmd+1").key(), Key::Char('1'));
        assert_eq!(trigger("cmd+\u{e9}").key(), Key::Char('\u{e9}'));
        assert_eq!(trigger("cmd+\u{c9}").key(), Key::Char('\u{e9}'));
        assert_eq!(trigger("cmd+f").key(), Key::Char('f'));

        // + and = as keys.
        assert_eq!(trigger("cmd++"), trigger("cmd+plus"));
        assert_eq!(
            trigger("+"),
            Trigger::new(Key::Char('+'), Mods::NONE).unwrap()
        );
        assert_eq!(trigger("cmd+shift++").mods(), CMD | Mods::SHIFT);
        assert_eq!(trigger("cmd+="), trigger("cmd+equal"));
    }

    #[test]
    fn bad_triggers_are_rejected() {
        assert_eq!(trigger_error(""), "missing trigger");
        assert_eq!(trigger_error("  "), "missing trigger");
        assert!(trigger_error("cmd+").contains("no key after its last +"));
        assert!(trigger_error("hyper+x").contains("unknown modifier \"hyper\""));
        assert!(trigger_error("cmd++x").contains("unknown modifier \"\""));
        assert!(trigger_error("cmd+frob").contains("unknown key \"frob\""));
        assert!(trigger_error("cmd+f25").contains("unknown key"));
        assert!(trigger_error("cmd+f0").contains("unknown key"));
        assert!(trigger_error("cmd+kp_10").contains("unknown key"));
        assert!(trigger_error("cmd+\u{1}").contains("cannot be bound"));
        assert!(trigger_error("cmd+shift").contains("unknown key \"shift\""));
    }

    #[test]
    fn trigger_constructor_normalizes_and_refuses_unbindable_keys() {
        let t = Trigger::new(Key::Char('Q'), CMD | Mods::CAPS_LOCK).unwrap();
        assert_eq!((t.key(), t.mods()), (Key::Char('q'), CMD));
        assert_eq!(Trigger::new(Key::Char('\t'), CMD), None);
        assert_eq!(Trigger::new(Key::F(0), CMD), None);
        assert_eq!(Trigger::new(Key::F(25), CMD), None);
        assert_eq!(Trigger::new(Key::Keypad(KeypadKey::Digit(10)), CMD), None);
        for key in [Key::CapsLock, Key::Menu, Key::ShiftLeft, Key::SuperRight] {
            assert_eq!(Trigger::new(key, Mods::NONE), None, "{key:?}");
        }
    }

    #[test]
    fn triggers_round_trip() {
        let mut keys: Vec<Key> = KEY_NAMES.iter().map(|&(_, k)| k).collect();
        keys.extend((1..=24).map(Key::F));
        keys.extend((0..=9).map(|d| Key::Keypad(KeypadKey::Digit(d))));
        keys.extend(
            "abz09[]/\\;'\u{60}.,=+- #\"\u{e9}\u{1f600}"
                .chars()
                .map(Key::Char),
        );
        for key in keys {
            for bits in 0..16 {
                let t = Trigger::new(key, Mods(bits)).unwrap();
                assert_eq!(trigger(&t.to_string()), t, "{t}");
            }
        }
        let canonical = [
            "cmd+ctrl+alt+shift+a",
            "cmd+[",
            "cmd+shift+]",
            "ctrl+shift+tab",
            "cmd+plus",
            "cmd+equal",
            "cmd+minus",
            "cmd+comma",
            "alt+space",
            "shift+page_down",
            "f13",
            "kp_7",
            "cmd+kp_enter",
            "cmd+backspace",
            "cmd+\\",
            "cmd+.",
        ];
        for text in canonical {
            assert_eq!(trigger(text).to_string(), text);
        }
    }

    #[test]
    fn keybind_values_bind_unbind_and_clear() {
        let mut b = Bindings::default();
        let before = b.len();

        // Rebinding a trigger replaces its action in place.
        b.apply("cmd+t=new_window").unwrap();
        assert_eq!(b.len(), before);
        assert_eq!(b.lookup(Key::Char('t'), CMD), Some(&Action::NewWindow));
        assert_eq!(b.iter().nth(1).map(|(t, _)| t), Some(trigger("cmd+t")));

        // A new trigger is added at the end.
        b.apply("ctrl+alt+x = text:hi").unwrap();
        assert_eq!(b.len(), before + 1);
        let last = b.iter().last().map(|(t, a)| (t, a.clone()));
        assert_eq!(
            last,
            Some((trigger("ctrl+alt+x"), Action::Text(b"hi".to_vec())))
        );

        // Spaces at the end of a text action are kept (the line parser has
        // already trimmed what was outside quotes).
        b.apply("cmd+y=text:a ").unwrap();
        assert_eq!(
            b.lookup(Key::Char('y'), CMD),
            Some(&Action::Text(b"a ".to_vec()))
        );

        // = as the key.
        b.apply("cmd+==ignore").unwrap();
        assert_eq!(b.lookup(Key::Char('='), CMD), Some(&Action::Ignore));
        b.apply("==new_tab").unwrap();
        assert_eq!(b.lookup(Key::Char('='), Mods::NONE), Some(&Action::NewTab));
        b.apply("cmd+x=text:a=b").unwrap();
        assert_eq!(
            b.lookup(Key::Char('x'), CMD),
            Some(&Action::Text(b"a=b".to_vec()))
        );

        // unbind removes one trigger; unbinding an unbound trigger is fine.
        b.apply("cmd+d=unbind").unwrap();
        assert_eq!(b.lookup(Key::Char('d'), CMD), None);
        b.apply("cmd+d=unbind").unwrap();
        assert_eq!(b.unbind(trigger("cmd+q")), Some(Action::Quit));
        assert_eq!(b.unbind(trigger("cmd+q")), None);

        // clear removes everything bound so far.
        b.apply("clear").unwrap();
        assert!(b.is_empty());
        assert_eq!(b, Bindings::empty());
        b.apply("cmd+t=new_tab").unwrap();
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn bad_keybind_values_are_rejected() {
        let mut b = Bindings::default();
        let cases = [
            ("cmd+t", "expected <trigger>=<action>"),
            ("cmd+=", "expected <trigger>=<action>"),
            ("=", "expected <trigger>=<action>"),
            ("cmd+t=", "unknown action \"\""),
            ("cmd+t=frobnicate", "unknown action"),
            ("cmd+t=split:sideways", "expected one of"),
            ("hyper+t=new_tab", "unknown modifier"),
            ("=new_tab", "expected <trigger>=<action>"),
            ("cmd+ =new_tab", "no key after its last +"),
            ("cmd+t=text:\\q", "unknown escape"),
        ];
        for (value, message) in cases {
            let error = b.apply(value).expect_err(value);
            assert!(error.contains(message), "{value}: {error}");
        }
        assert_eq!(b, Bindings::default(), "failed values must change nothing");
    }
    #[test]
    fn the_plus_key_can_come_right_before_the_separator() {
        let mut b = Bindings::empty();
        b.apply("cmd++=font_size:decrease").unwrap();
        b.apply("+=new_tab").unwrap();
        b.apply("shift++=text:+").unwrap();
        b.apply("ctrl + + = new_window").unwrap();
        b.apply("alt++ = text:a=b").unwrap();
        assert_eq!(
            b.lookup(Key::Char('+'), CMD),
            Some(&Action::FontSize(FontSizeChange::Decrease))
        );
        assert_eq!(b.lookup(Key::Char('+'), Mods::NONE), Some(&Action::NewTab));
        assert_eq!(
            b.lookup(Key::Char('+'), Mods::SHIFT),
            Some(&Action::Text(b"+".to_vec()))
        );
        assert_eq!(
            b.lookup(Key::Char('+'), Mods::CTRL),
            Some(&Action::NewWindow)
        );
        assert_eq!(
            b.lookup(Key::Char('+'), Mods::ALT),
            Some(&Action::Text(b"a=b".to_vec()))
        );
        assert_eq!(b.len(), 5);
        // The = key is unaffected.
        b.apply("cmd+= = ignore").unwrap();
        assert_eq!(b.lookup(Key::Char('='), CMD), Some(&Action::Ignore));
        assert_eq!(trigger("cmd + +"), trigger("cmd+plus"));
        assert_eq!(trigger("cmd+ +"), trigger("cmd+plus"));
    }

    #[test]
    fn spaces_other_than_the_space_bar_cannot_be_bound() {
        // The parser trims them, so they could never be read back.
        for c in ['\u{a0}', '\u{2003}', '\u{2028}', '\u{3000}'] {
            assert_eq!(Trigger::new(Key::Char(c), CMD), None, "{c:?}");
        }
        assert!(Trigger::new(Key::Char(' '), CMD).is_some());
    }

    /// Every character a trigger accepts, under several modifier sets, reads
    /// back from its spelling, both on its own and in a config line.
    #[test]
    fn every_bindable_character_round_trips_through_a_config_line() {
        let mods = [Mods::NONE, Mods::SHIFT, CMD, Mods(0b1111)];
        let chars = (0..=0xffff_u32)
            .chain((0x10000..=0x10ffff).step_by(97))
            .filter_map(char::from_u32);
        let mut checked = 0;
        for c in chars {
            for &m in &mods {
                let Some(t) = Trigger::new(Key::Char(c), m) else {
                    continue;
                };
                let text = t.to_string();
                assert_eq!(text.parse::<Trigger>().ok(), Some(t), "{c:?} as {text:?}");
                let line = format!("keybind = {text}=new_tab");
                let mut settings = crate::config::settings(line.as_bytes());
                let Some((_, Ok(("keybind", value)))) = settings.next() else {
                    panic!("{line:?} is not a keybind setting");
                };
                let mut b = Bindings::empty();
                b.apply(value).unwrap_or_else(|e| panic!("{line:?}: {e}"));
                assert_eq!(
                    b.lookup(t.key(), t.mods()),
                    Some(&Action::NewTab),
                    "{line:?}"
                );
                checked += 1;
            }
        }
        assert!(checked > 200_000, "{checked}");
    }
}
