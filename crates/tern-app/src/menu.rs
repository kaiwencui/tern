//! The main menu. Every item performs an [AppCommand], and items for
//! configurable actions show the shortcut the key bindings give them.

use std::borrow::Cow;

use objc2::{sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;
use tern_config::{
    Action, Bindings, Direction, FocusTarget, FontSizeChange, PromptDirection, ScrollTarget,
};
use tern_types::{Key, Mods};

use crate::identity;

/// What a menu item or an automation request can ask for: a configurable
/// action, or one of the standard macOS commands that have no binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppCommand {
    Do(Action),
    About,
    Hide,
    HideOthers,
    ShowAll,
    Minimize,
    Zoom,
    BringAllToFront,
}

/// The standard commands by name, for the automation socket.
const NAMED: [(&str, AppCommand); 7] = [
    ("about", AppCommand::About),
    ("hide", AppCommand::Hide),
    ("hide_others", AppCommand::HideOthers),
    ("show_all", AppCommand::ShowAll),
    ("minimize", AppCommand::Minimize),
    ("zoom", AppCommand::Zoom),
    ("bring_all_to_front", AppCommand::BringAllToFront),
];

impl AppCommand {
    /// A standard command's name, or an action as a config file writes it.
    pub fn parse(text: &str) -> Option<AppCommand> {
        match NAMED.iter().find(|(name, _)| *name == text) {
            Some((_, command)) => Some(command.clone()),
            None => text.parse().ok().map(AppCommand::Do),
        }
    }

    pub fn name(&self) -> String {
        match self {
            AppCommand::Do(action) => action.to_string(),
            other => NAMED
                .iter()
                .find(|(_, c)| c == other)
                .map_or_else(String::new, |(name, _)| (*name).to_string()),
        }
    }

    /// Whether the command acts on a window, and so needs one to exist.
    pub fn needs_window(&self) -> bool {
        match self {
            AppCommand::Do(action) => !matches!(
                action,
                Action::NewWindow
                    | Action::NewTab
                    | Action::OpenConfig
                    | Action::ReloadConfig
                    | Action::Quit
                    | Action::Ignore
            ),
            AppCommand::Minimize | AppCommand::Zoom => true,
            _ => false,
        }
    }

    /// The shortcut of a standard command; configurable actions take theirs
    /// from the bindings.
    fn fixed_shortcut(&self) -> Option<(Key, Mods)> {
        match self {
            AppCommand::Hide => Some((Key::Char('h'), Mods::SUPER)),
            AppCommand::HideOthers => Some((Key::Char('h'), Mods::SUPER | Mods::ALT)),
            AppCommand::Minimize => Some((Key::Char('m'), Mods::SUPER)),
            _ => None,
        }
    }
}

enum Entry {
    Item(Cow<'static, str>, AppCommand),
    Separator,
}

fn item(title: impl Into<Cow<'static, str>>, command: AppCommand) -> Entry {
    Entry::Item(title.into(), command)
}

fn act(title: impl Into<Cow<'static, str>>, action: Action) -> Entry {
    Entry::Item(title.into(), AppCommand::Do(action))
}

/// The menus and their items. The first menu's title is replaced by the
/// app name.
fn layout(app_name: &str) -> Vec<(Cow<'static, str>, Vec<Entry>)> {
    use Entry::Separator;
    vec![
        (
            Cow::Owned(app_name.to_owned()),
            vec![
                item(format!("About {app_name}"), AppCommand::About),
                Separator,
                act("Settings…", Action::OpenConfig),
                act("Reload Configuration", Action::ReloadConfig),
                Separator,
                item(format!("Hide {app_name}"), AppCommand::Hide),
                item("Hide Others", AppCommand::HideOthers),
                item("Show All", AppCommand::ShowAll),
                Separator,
                act(format!("Quit {app_name}"), Action::Quit),
            ],
        ),
        (
            Cow::Borrowed("Shell"),
            vec![
                act("New Window", Action::NewWindow),
                act("New Tab", Action::NewTab),
                Separator,
                act("Split Right", Action::Split(Direction::Right)),
                act("Split Left", Action::Split(Direction::Left)),
                act("Split Down", Action::Split(Direction::Down)),
                act("Split Up", Action::Split(Direction::Up)),
                Separator,
                act("Close", Action::ClosePane),
                act("Close Tab", Action::CloseTab),
                act("Close Window", Action::CloseWindow),
            ],
        ),
        (
            Cow::Borrowed("Edit"),
            vec![
                act("Copy", Action::Copy),
                act("Paste", Action::Paste),
                act("Select All", Action::SelectAll),
                act("Select Last Command Output", Action::SelectCommandOutput),
                Separator,
                act("Find…", Action::Find),
                act("Find Next", Action::FindNext),
                act("Find Previous", Action::FindPrevious),
                Separator,
                act("Clear", Action::Clear),
            ],
        ),
        (
            Cow::Borrowed("View"),
            vec![
                act("Show/Hide Tab Bar", Action::ToggleTabBar),
                Separator,
                act("Bigger", Action::FontSize(FontSizeChange::Increase)),
                act("Smaller", Action::FontSize(FontSizeChange::Decrease)),
                act("Actual Size", Action::FontSize(FontSizeChange::Reset)),
                Separator,
                act("Previous Prompt", Action::JumpPrompt(PromptDirection::Previous)),
                act("Next Prompt", Action::JumpPrompt(PromptDirection::Next)),
                Separator,
                act("Page Up", Action::Scroll(ScrollTarget::PageUp)),
                act("Page Down", Action::Scroll(ScrollTarget::PageDown)),
                act("Scroll to Top", Action::Scroll(ScrollTarget::Top)),
                act("Scroll to Bottom", Action::Scroll(ScrollTarget::Bottom)),
                Separator,
                act("Toggle Full Screen", Action::ToggleFullscreen),
            ],
        ),
        (
            Cow::Borrowed("Window"),
            vec![
                item("Minimize", AppCommand::Minimize),
                item("Zoom", AppCommand::Zoom),
                Separator,
                act("Rename Tab…", Action::RenameTab),
                act("Show Previous Tab", Action::PreviousTab),
                act("Show Next Tab", Action::NextTab),
                act("Move Tab to New Window", Action::MoveTabToNewWindow),
                act("Merge All Windows", Action::MergeAllWindows),
                Separator,
                act(
                    "Select Previous Split",
                    Action::FocusSplit(FocusTarget::Previous),
                ),
                act("Select Next Split", Action::FocusSplit(FocusTarget::Next)),
                act("Zoom Split", Action::ToggleZoom),
                act("Equalize Splits", Action::EqualizeSplits),
                Separator,
                item("Bring All to Front", AppCommand::BringAllToFront),
            ],
        ),
    ]
}

/// Builds the main menu. Each item's tag, less one, indexes the returned
/// commands (AppKit's own items have tag 0); target is the object whose
/// performAction: carries them out.
pub fn install(
    mtm: MainThreadMarker,
    ns: &NSApplication,
    bindings: &Bindings,
    target: &objc2::runtime::AnyObject,
) -> Vec<AppCommand> {
    let main = NSMenu::new(mtm);
    let mut commands = Vec::new();
    for (title, entries) in layout(identity::app_name()) {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(&title));
        for entry in entries {
            let (title, command) = match entry {
                Entry::Separator => {
                    menu.addItem(&NSMenuItem::separatorItem(mtm));
                    continue;
                }
                Entry::Item(title, command) => (title, command),
            };
            let (key, mods) = shortcut(&command, bindings);
            // SAFETY: performAction: is defined on the target and takes the sender.
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(&title),
                    Some(sel!(performAction:)),
                    &NSString::from_str(&key),
                )
            };
            item.setKeyEquivalentModifierMask(mods);
            // SAFETY: the target lives as long as the app.
            unsafe { item.setTarget(Some(target)) };
            item.setTag(commands.len() as isize + 1);
            commands.push(command);
            menu.addItem(&item);
        }
        let holder = NSMenuItem::new(mtm);
        holder.setSubmenu(Some(&menu));
        main.addItem(&holder);
        if title == "Window" {
            // AppKit adds its tab and window list items to this menu.
            ns.setWindowsMenu(Some(&menu));
        }
    }
    ns.setMainMenu(Some(&main));
    commands
}

/// Gives every item the shortcut the bindings now give its command, after
/// the configuration was reloaded. commands is what install returned.
pub fn update_shortcuts(ns: &NSApplication, bindings: &Bindings, commands: &[AppCommand]) {
    let Some(main) = ns.mainMenu() else {
        return;
    };
    for top in main.itemArray().iter() {
        let Some(menu) = top.submenu() else { continue };
        for item in menu.itemArray().iter() {
            let command = usize::try_from(item.tag())
                .ok()
                .and_then(|tag| tag.checked_sub(1))
                .and_then(|i| commands.get(i));
            if let Some(command) = command {
                let (key, mods) = shortcut(command, bindings);
                item.setKeyEquivalent(&NSString::from_str(&key));
                item.setKeyEquivalentModifierMask(mods);
            }
        }
    }
}

/// A command's menu shortcut: its first binding, or the standard one.
fn shortcut(command: &AppCommand, bindings: &Bindings) -> (String, NSEventModifierFlags) {
    let shortcut = match command {
        AppCommand::Do(action) => bindings.shortcut_for(action),
        other => other.fixed_shortcut(),
    };
    shortcut
        .and_then(|(key, mods)| key_equivalent(key, mods))
        .unwrap_or_else(|| (String::new(), NSEventModifierFlags::empty()))
}

/// A binding's trigger as a menu item's key equivalent and modifier mask.
/// None for keys a menu cannot show (keypad keys, function keys past F35).
pub fn key_equivalent(key: Key, mods: Mods) -> Option<(String, NSEventModifierFlags)> {
    let c = match key {
        Key::Char(c) => c,
        Key::Enter => '\r',
        Key::Tab => '\t',
        Key::Backspace => '\u{8}',
        Key::Escape => '\u{1b}',
        Key::Up => '\u{F700}',
        Key::Down => '\u{F701}',
        Key::Left => '\u{F702}',
        Key::Right => '\u{F703}',
        Key::Insert => '\u{F727}',
        Key::Delete => '\u{F728}',
        Key::Home => '\u{F729}',
        Key::End => '\u{F72B}',
        Key::PageUp => '\u{F72C}',
        Key::PageDown => '\u{F72D}',
        Key::F(n @ 1..=35) => char::from_u32(0xF704 + u32::from(n) - 1)?,
        _ => return None,
    };
    let mut mask = NSEventModifierFlags::empty();
    for (bit, flag) in [
        (Mods::SHIFT, NSEventModifierFlags::Shift),
        (Mods::ALT, NSEventModifierFlags::Option),
        (Mods::CTRL, NSEventModifierFlags::Control),
        (Mods::SUPER, NSEventModifierFlags::Command),
    ] {
        if mods.contains(bit) {
            mask |= flag;
        }
    }
    Some((c.to_string(), mask))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names_round_trip() {
        for (name, command) in NAMED {
            assert_eq!(AppCommand::parse(name), Some(command.clone()));
            assert_eq!(command.name(), name);
        }
        for (_, entries) in layout("Tern") {
            for entry in entries {
                if let Entry::Item(title, command) = entry {
                    assert_eq!(AppCommand::parse(&command.name()), Some(command), "{title}");
                }
            }
        }
        assert_eq!(
            AppCommand::parse("goto_tab:2"),
            Some(AppCommand::Do(Action::GotoTab(2)))
        );
        assert_eq!(AppCommand::parse("nope"), None);
    }

    #[test]
    fn app_menu_uses_runtime_app_name() {
        let menus = layout("Tern Next");
        let (title, entries) = &menus[0];
        assert_eq!(title, "Tern Next");
        let item_titles: Vec<&str> = entries
            .iter()
            .filter_map(|e| match e {
                Entry::Item(t, _) => Some(t.as_ref()),
                Entry::Separator => None,
            })
            .collect();
        assert!(item_titles.contains(&"About Tern Next"));
        assert!(item_titles.contains(&"Hide Tern Next"));
        assert!(item_titles.contains(&"Quit Tern Next"));
    }

    #[test]
    fn shortcuts_become_key_equivalents() {
        let cmd = NSEventModifierFlags::Command;
        assert_eq!(
            key_equivalent(Key::Char('t'), Mods::SUPER),
            Some(("t".into(), cmd))
        );
        assert_eq!(
            key_equivalent(Key::Char(']'), Mods::SUPER | Mods::SHIFT),
            Some(("]".into(), cmd | NSEventModifierFlags::Shift))
        );
        assert_eq!(
            key_equivalent(Key::Left, Mods::SUPER | Mods::ALT),
            Some(("\u{F702}".into(), cmd | NSEventModifierFlags::Option))
        );
        assert_eq!(
            key_equivalent(Key::F(5), Mods::CTRL),
            Some(("\u{F708}".into(), NSEventModifierFlags::Control))
        );
        assert_eq!(
            key_equivalent(Key::Enter, Mods::NONE).map(|k| k.0),
            Some("\r".into())
        );
        assert_eq!(
            key_equivalent(Key::Keypad(tern_types::KeypadKey::Add), Mods::SUPER),
            None
        );
    }

    #[test]
    fn default_bindings_give_the_expected_menu_shortcuts() {
        let b = Bindings::default();
        let shown = |action: Action| {
            b.shortcut_for(&action)
                .and_then(|(k, m)| key_equivalent(k, m))
                .map(|(k, m)| (k, m.0))
        };
        let cmd = NSEventModifierFlags::Command.0;
        assert_eq!(shown(Action::NewTab), Some(("t".into(), cmd)));
        assert_eq!(
            shown(Action::FontSize(FontSizeChange::Increase)),
            Some(("=".into(), cmd))
        );
        assert_eq!(shown(Action::Quit), Some(("q".into(), cmd)));
    }
}
