//! perform(): what every action, menu item and automation command does.

use std::rc::Rc;

use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{NSApplication, NSWindow};
use tern_config::{Action, FontSizeChange, PromptDirection, ScrollTarget};
use tern_core::Scroll;

use super::{App, CloseTarget, Placement, MAX_FONT_SIZE, MIN_FONT_SIZE};
use crate::menu::AppCommand;
use crate::state::WindowState;

impl App {
    /// Carries out an action. window names the window it came from (a key
    /// press); otherwise it acts on the current window.
    pub fn perform(&self, action: Action, window: Option<u64>) {
        self.run_command(AppCommand::Do(action), window);
    }

    pub fn run_command(&self, command: AppCommand, window: Option<u64>) {
        let ns = NSApplication::sharedApplication(self.mtm);
        let target = window
            .and_then(|id| self.window(id))
            .or_else(|| self.current_window());
        let state = target
            .as_ref()
            .and_then(|w| self.id_of(w))
            .and_then(|id| self.state(id));
        match command {
            AppCommand::About => ns.orderFrontStandardAboutPanel(None),
            AppCommand::Hide => ns.hide(None),
            AppCommand::HideOthers => ns.hideOtherApplications(None),
            AppCommand::ShowAll => ns.unhideAllApplications(None),
            AppCommand::BringAllToFront => ns.arrangeInFront(None),
            AppCommand::Minimize => {
                if let Some(w) = &target {
                    w.performMiniaturize(None);
                }
            }
            AppCommand::Zoom => {
                if let Some(w) = &target {
                    w.performZoom(None);
                }
            }
            AppCommand::Do(action) => self.perform_action(action, target, state),
        }
    }

    fn perform_action(
        &self,
        action: Action,
        window: Option<Retained<NSWindow>>,
        state: Option<Rc<WindowState>>,
    ) {
        if window
            .as_ref()
            .is_some_and(|w| self.rename_edit(&action, w))
        {
            return;
        }
        match action {
            Action::NewWindow => {
                let dir = self.new_pane_dir(state.as_deref());
                self.create_window(Placement::Window, None, dir);
            }
            Action::NewTab => {
                let dir = self.new_pane_dir(state.as_deref());
                let placement = window.map_or(Placement::Window, Placement::TabOf);
                self.create_window(placement, None, dir);
            }
            Action::Quit => self.quit(),
            Action::OpenConfig => self.open_config(),
            Action::ReloadConfig => self.reload_config(),
            Action::Ignore => {}
            action => {
                let (Some(window), Some(state)) = (window, state) else {
                    return;
                };
                self.window_action(action, &window, &state);
            }
        }
    }

    fn window_action(&self, action: Action, window: &NSWindow, state: &WindowState) {
        match action {
            Action::ClosePane => {
                if let Some(pane) = state.focused_pane_id() {
                    self.request_close(CloseTarget::Pane(state.id, pane));
                }
            }
            Action::CloseTab => {
                self.request_close(CloseTarget::Tab(state.id));
            }
            Action::CloseWindow => {
                self.request_close(CloseTarget::Window(state.id));
            }
            Action::Split(side) => state.split(side, self.new_pane_dir(Some(state))),
            Action::FocusSplit(target) => state.focus_split(target),
            Action::ResizeSplit(side, points) => state.resize_split(side, points),
            Action::EqualizeSplits => state.equalize(),
            Action::ToggleZoom => state.toggle_zoom(),
            Action::GotoTab(9) => select_tab(window, TabChoice::Last),
            Action::GotoTab(n) => select_tab(window, TabChoice::Number(n)),
            Action::NextTab => select_tab(window, TabChoice::Offset(1)),
            Action::PreviousTab => select_tab(window, TabChoice::Offset(-1)),
            Action::MoveTab(by) => move_tab(window, by),
            Action::ToggleTabBar => {
                window.toggleTabBar(None);
                state.sync_tab_activity(false);
                state.reapply_chrome();
                crate::window::reapply_chrome_soon(state.id);
            }
            Action::MergeAllWindows => {
                window.mergeAllWindows(None);
                for (st, _, _) in self.all_windows() {
                    st.sync_tab_activity(true);
                }
                state.reapply_chrome();
                crate::window::reapply_chrome_soon(state.id);
            }
            Action::MoveTabToNewWindow => {
                window.moveTabToNewWindow(None);
                state.reapply_chrome();
                crate::window::reapply_chrome_soon(state.id);
            }
            Action::RenameTab => self.rename_tab_showing_bar(window, state.id),
            Action::Copy => {
                if let Some(id) = state.focused_pane_id() {
                    state.copy_selection(id);
                }
            }
            Action::Paste => {
                if let Some(text) = self.clipboard.text() {
                    if state.search_open() {
                        state.paste_into_find(&text);
                    } else {
                        state.paste(&text);
                    }
                }
            }
            Action::SelectAll => {
                if !state.search_open() {
                    state.select_all();
                }
            }
            Action::SelectCommandOutput => state.select_last_output(),
            Action::Find => state.open_find(),
            Action::FindNext => state.find_step(1),
            Action::FindPrevious => state.find_step(-1),
            Action::Clear => state.clear(),
            Action::JumpPrompt(dir) => state.jump_prompt(match dir {
                PromptDirection::Previous => -1,
                PromptDirection::Next => 1,
            }),
            Action::Scroll(target) => state.scroll(match target {
                ScrollTarget::PageUp => Scroll::Pages(1),
                ScrollTarget::PageDown => Scroll::Pages(-1),
                ScrollTarget::Top => Scroll::Top,
                ScrollTarget::Bottom => Scroll::Bottom,
                ScrollTarget::LineUp => Scroll::Lines(1),
                ScrollTarget::LineDown => Scroll::Lines(-1),
            }),
            Action::ToggleFullscreen => window.toggleFullScreen(None),
            Action::FontSize(change) => self.change_window_font_size(window, state, change),
            Action::Text(bytes) => {
                if state.search_open() {
                    if bytes == [0x15] {
                        state.clear_find_query();
                    }
                } else {
                    state.send(&bytes);
                }
            }
            // perform_action handles these before calling here.
            Action::NewWindow
            | Action::NewTab
            | Action::Quit
            | Action::OpenConfig
            | Action::ReloadConfig
            | Action::Ignore => {}
        }
    }

    /// Adjusts the font size of `window` (and any sibling tabs in the same tab
    /// group) without altering other windows.
    fn change_window_font_size(
        &self,
        window: &NSWindow,
        state: &WindowState,
        change: FontSizeChange,
    ) {
        let cur = state.font_size();
        let next = match change {
            FontSizeChange::Increase => (cur + 1.0).min(MAX_FONT_SIZE),
            FontSizeChange::Decrease => (cur - 1.0).max(MIN_FONT_SIZE),
            FontSizeChange::Reset => self.config().font_size,
        };
        if (next - cur).abs() < 0.001 {
            return;
        }
        if let Some(tabs) = window.tabbedWindows() {
            for tab in tabs.iter() {
                if let Some(id) = self.id_of(&tab)
                    && let Some(tab_state) = self.state(id) {
                        tab_state.set_font_size(next);
                    }
            }
        } else {
            state.set_font_size(next);
        }
    }

    /// The command of the menu item with this tag; None for items AppKit
    /// added itself, whose tag is 0.
    pub fn menu_command(&self, tag: isize) -> Option<AppCommand> {
        let index = usize::try_from(tag).ok()?.checked_sub(1)?;
        self.menu_commands.borrow().get(index).cloned()
    }

    pub(super) fn perform_menu_item(&self, tag: isize) {
        if let Some(command) = self.menu_command(tag) {
            self.run_command(command, None);
        }
    }

    pub(super) fn validate_menu_item(&self, tag: isize) -> bool {
        match self.menu_command(tag) {
            // Edit items are enabled only when they have something to act
            // on: the tab's name while one is being typed, or the terminal.
            Some(AppCommand::Do(Action::Copy)) => self.current_window().is_some_and(|w| {
                self.name_selection(&w).unwrap_or_else(|| {
                    self.id_of(&w)
                        .and_then(|id| self.state(id))
                        .is_some_and(|s| s.has_selection())
                })
            }),
            Some(AppCommand::Do(Action::Paste)) => {
                self.current_window().is_some() && self.clipboard.has_text()
            }
            Some(AppCommand::Do(Action::MoveTabToNewWindow)) => self
                .current_window()
                .and_then(|w| w.tabbedWindows())
                .is_some_and(|t| t.len() > 1),
            Some(AppCommand::Do(Action::MergeAllWindows)) => self.has_multiple_window_groups(),
            Some(command) if command.needs_window() => self.current_window().is_some(),
            Some(_) => true,
            None => false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum TabChoice {
    /// Counting from 1; 0 and numbers past the last tab select nothing.
    Number(u8),
    Last,
    /// Relative to the window's own tab, wrapping around.
    Offset(isize),
}

/// Selects a tab in the window's tab group. Works through the group itself
/// rather than selectNextTab:, which does nothing while the app is inactive,
/// and asks the group each time because the user can drag tabs between
/// windows.
fn select_tab(window: &NSWindow, choice: TabChoice) {
    let Some(group) = window.tabGroup() else {
        return;
    };
    let tabs = group.windows().to_vec();
    let here = tabs.iter().position(|t| std::ptr::eq(&**t, window));
    if let Some(target) = tab_index(tabs.len(), here, choice).and_then(|i| tabs.get(i)) {
        group.setSelectedWindow(Some(target));
        keep_in_front(target);
    }
}

/// A tab that became the one shown takes the place of the one it replaced.
/// In an inactive app AppKit orders it behind the active app's windows,
/// where the window that was showing had been brought up without taking
/// focus (see App::show), so it is brought up the same way.
pub fn keep_in_front(window: &NSWindow) {
    if !NSApplication::sharedApplication(window.mtm()).isActive() {
        window.orderFrontRegardless();
    }
}

/// Moves the window's tab by places in its tab bar (negative: left),
/// stopping at either end. It stays the selected tab.
fn move_tab(window: &NSWindow, by: i8) {
    let Some(group) = window.tabGroup() else {
        return;
    };
    let tabs = group.windows().to_vec();
    let Some(here) = tabs.iter().position(|t| std::ptr::eq(&**t, window)) else {
        return;
    };
    let to = moved_index(tabs.len(), here, by);
    if to == here {
        return;
    }
    let selected = group.selectedWindow();
    group.removeWindow(window);
    // to is below the count, which fits an NSInteger.
    group.insertWindow_atIndex(window, to as isize);
    if let Some(selected) = selected {
        group.setSelectedWindow(Some(&selected));
        keep_in_front(&selected);
    }
}

/// Where a tab at here ends up among count tabs after moving by places.
fn moved_index(count: usize, here: usize, by: i8) -> usize {
    let last = count.saturating_sub(1);
    if by < 0 {
        here.saturating_sub(usize::from(by.unsigned_abs()))
    } else {
        here.saturating_add(usize::from(by.unsigned_abs()))
            .min(last)
    }
}

/// The index of the tab to select among count tabs, the window's own tab
/// being at here.
fn tab_index(count: usize, here: Option<usize>, choice: TabChoice) -> Option<usize> {
    let index = match choice {
        TabChoice::Number(n) => usize::from(n).checked_sub(1)?,
        TabChoice::Last => count.checked_sub(1)?,
        TabChoice::Offset(by) => {
            let n = isize::try_from(count).ok().filter(|&n| n > 0)?;
            let i = isize::try_from(here?).ok().filter(|&i| i < n)?;
            // Both terms are below n, so the sum cannot overflow.
            usize::try_from((i + by.rem_euclid(n)).rem_euclid(n)).ok()?
        }
    };
    (index < count).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_tabs_stop_at_either_end() {
        assert_eq!(moved_index(4, 1, 1), 2);
        assert_eq!(moved_index(4, 1, -1), 0);
        assert_eq!(moved_index(4, 1, -5), 0);
        assert_eq!(moved_index(4, 1, 100), 3);
        assert_eq!(moved_index(4, 3, i8::MAX), 3);
        assert_eq!(moved_index(4, 3, i8::MIN), 0);
        assert_eq!(moved_index(1, 0, 1), 0);
    }

    #[test]
    fn tab_choice_picks_the_right_index() {
        use TabChoice::*;
        assert_eq!(tab_index(4, Some(0), Number(1)), Some(0));
        assert_eq!(tab_index(4, Some(0), Number(4)), Some(3));
        assert_eq!(tab_index(4, Some(0), Number(5)), None);
        assert_eq!(tab_index(4, Some(0), Number(0)), None);
        assert_eq!(tab_index(4, None, Number(2)), Some(1));
        assert_eq!(tab_index(4, Some(2), Last), Some(3));
        assert_eq!(tab_index(0, None, Last), None);
        assert_eq!(tab_index(4, Some(3), Offset(1)), Some(0), "next wraps");
        assert_eq!(tab_index(4, Some(0), Offset(-1)), Some(3), "previous wraps");
        assert_eq!(tab_index(4, Some(1), Offset(1)), Some(2));
        assert_eq!(tab_index(1, Some(0), Offset(1)), Some(0));
        assert_eq!(
            tab_index(4, None, Offset(1)),
            None,
            "the window left the group"
        );
        assert_eq!(tab_index(0, Some(0), Offset(1)), None);
        assert_eq!(tab_index(3, Some(1), Offset(isize::MAX)), Some(2));
        assert_eq!(tab_index(3, Some(1), Offset(isize::MIN)), Some(2));
    }
}
