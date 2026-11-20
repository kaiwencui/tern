//! Native right-click context menus for panes and tabs.

use objc2::rc::Retained;
use objc2::{sel, MainThreadOnly};
use objc2_app_kit::{NSEvent, NSMenu, NSMenuItem, NSView, NSWindow};
use objc2_foundation::NSString;
use tern_config::{Action, Direction};
use tern_types::PaneId;

use super::{App, CloseTarget};
use crate::links::ClickTarget;

pub struct PaneContext {
    pub pane: PaneId,
    pub target: Option<ClickTarget>,
    pub has_selection: bool,
    pub pane_count: usize,
    pub is_zoomed: bool,
}

#[derive(Clone, Debug)]
pub(super) enum ContextAction {
    OpenTarget(ClickTarget),
    CopyText(String),
    Do { window: u64, action: Action },
    RenameTab(u64),
    NewTabNextTo(u64),
    MoveTabToNewWindow(u64),
    MergeAllWindows(u64),
    CloseTab(u64),
    CloseOtherTabs(u64),
}

impl App {
    pub fn show_pane_context_menu(
        &self,
        view: &NSView,
        event: &NSEvent,
        window_id: u64,
        ctx: PaneContext,
    ) {
        if self.records_input() {
            return;
        }
        let mut actions = Vec::new();
        let menu = self.new_context_menu();

        if let Some(target) = ctx.target {
            let (open_label, copy_label, copy_text) = match &target {
                ClickTarget::Url(url) => ("Open Link", "Copy Link", url.clone()),
                ClickTarget::File(file) => (
                    "Open File",
                    "Copy Path",
                    file.path.to_string_lossy().into_owned(),
                ),
            };
            self.add_context_item(
                &menu,
                &mut actions,
                open_label,
                true,
                ContextAction::OpenTarget(target),
            );
            self.add_context_item(
                &menu,
                &mut actions,
                copy_label,
                true,
                ContextAction::CopyText(copy_text),
            );
            menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        }

        self.add_context_item(
            &menu,
            &mut actions,
            "Copy",
            ctx.has_selection,
            ContextAction::Do {
                window: window_id,
                action: Action::Copy,
            },
        );
        self.add_context_item(
            &menu,
            &mut actions,
            "Paste",
            self.clipboard.has_text(),
            ContextAction::Do {
                window: window_id,
                action: Action::Paste,
            },
        );
        self.add_context_item(
            &menu,
            &mut actions,
            "Select All",
            true,
            ContextAction::Do {
                window: window_id,
                action: Action::SelectAll,
            },
        );
        self.add_context_item(
            &menu,
            &mut actions,
            "Select Last Command Output",
            true,
            ContextAction::Do {
                window: window_id,
                action: Action::SelectCommandOutput,
            },
        );
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));

        self.add_context_item(
            &menu,
            &mut actions,
            "Split Right",
            true,
            ContextAction::Do {
                window: window_id,
                action: Action::Split(Direction::Right),
            },
        );
        self.add_context_item(
            &menu,
            &mut actions,
            "Split Down",
            true,
            ContextAction::Do {
                window: window_id,
                action: Action::Split(Direction::Down),
            },
        );
        if ctx.pane_count > 1 || ctx.is_zoomed {
            self.add_context_item(
                &menu,
                &mut actions,
                if ctx.is_zoomed {
                    "Unzoom Split"
                } else {
                    "Zoom Split"
                },
                true,
                ContextAction::Do {
                    window: window_id,
                    action: Action::ToggleZoom,
                },
            );
            self.add_context_item(
                &menu,
                &mut actions,
                "Equalize Splits",
                true,
                ContextAction::Do {
                    window: window_id,
                    action: Action::EqualizeSplits,
                },
            );
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));

        self.add_context_item(
            &menu,
            &mut actions,
            "Find…",
            true,
            ContextAction::Do {
                window: window_id,
                action: Action::Find,
            },
        );
        self.add_context_item(
            &menu,
            &mut actions,
            "Clear",
            true,
            ContextAction::Do {
                window: window_id,
                action: Action::Clear,
            },
        );
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));

        self.add_context_item(
            &menu,
            &mut actions,
            if ctx.pane_count > 1 {
                "Close Pane"
            } else {
                "Close Tab"
            },
            true,
            ContextAction::Do {
                window: window_id,
                action: Action::ClosePane,
            },
        );

        let _ = ctx.pane;
        *self.context_actions.borrow_mut() = actions;
        NSMenu::popUpContextMenu_withEvent_forView(&menu, event, view);
    }

    pub fn show_tab_context_menu(
        &self,
        owner: &NSWindow,
        event_window: &NSWindow,
        event: &NSEvent,
        tab_id: u64,
    ) -> bool {
        if self.records_input() {
            return false;
        }
        let Some(view) = event_window
            .contentView()
            .or_else(|| owner.contentView())
        else {
            return false;
        };
        let tab_count = owner.tabbedWindows().map_or(1, |t| t.len());
        let multiple_groups = self.has_multiple_window_groups();
        let mut actions = Vec::new();
        let menu = self.new_context_menu();

        self.add_context_item(
            &menu,
            &mut actions,
            "Rename Tab…",
            true,
            ContextAction::RenameTab(tab_id),
        );
        self.add_context_item(
            &menu,
            &mut actions,
            "New Tab Here",
            true,
            ContextAction::NewTabNextTo(tab_id),
        );
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));

        self.add_context_item(
            &menu,
            &mut actions,
            "Move Tab to New Window",
            tab_count > 1,
            ContextAction::MoveTabToNewWindow(tab_id),
        );
        self.add_context_item(
            &menu,
            &mut actions,
            "Merge All Windows",
            multiple_groups,
            ContextAction::MergeAllWindows(tab_id),
        );
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));

        self.add_context_item(
            &menu,
            &mut actions,
            "Close Tab",
            true,
            ContextAction::CloseTab(tab_id),
        );
        if tab_count > 1 {
            self.add_context_item(
                &menu,
                &mut actions,
                "Close Other Tabs",
                true,
                ContextAction::CloseOtherTabs(tab_id),
            );
        }

        *self.context_actions.borrow_mut() = actions;
        NSMenu::popUpContextMenu_withEvent_forView(&menu, event, &view);
        true
    }

    pub(super) fn perform_context_action(&self, tag: isize) {
        let action = usize::try_from(tag)
            .ok()
            .and_then(|t| t.checked_sub(1))
            .and_then(|i| self.context_actions.borrow().get(i).cloned());
        let Some(action) = action else {
            return;
        };
        match action {
            ContextAction::OpenTarget(target) => self.open_target(&target),
            ContextAction::CopyText(text) => self.clipboard.set_text(&text),
            ContextAction::Do { window, action } => self.perform(action, Some(window)),
            ContextAction::RenameTab(id) => {
                if let Some(win) = self.window(id) {
                    self.rename_tab_showing_bar(&win, id);
                }
            }
            ContextAction::NewTabNextTo(id) => self.new_tab_next_to(id),
            ContextAction::MoveTabToNewWindow(id) => {
                if let Some(win) = self.window(id) {
                    win.moveTabToNewWindow(None);
                }
            }
            ContextAction::MergeAllWindows(id) => {
                if let Some(win) = self.window(id) {
                    win.mergeAllWindows(None);
                }
            }
            ContextAction::CloseTab(id) => self.request_close(CloseTarget::Tab(id)),
            ContextAction::CloseOtherTabs(keep_id) => {
                let Some(win) = self.window(keep_id) else {
                    return;
                };
                let others: Vec<u64> = win
                    .tabbedWindows()
                    .map(|tabs| {
                        tabs.iter()
                            .filter_map(|w| self.id_of(&w))
                            .filter(|&id| id != keep_id)
                            .collect()
                    })
                    .unwrap_or_default();
                for id in others {
                    self.request_close(CloseTarget::Tab(id));
                }
            }
        }
    }

    fn new_context_menu(&self) -> Retained<NSMenu> {
        let menu = NSMenu::new(self.mtm);
        menu.setAutoenablesItems(false);
        menu
    }

    fn add_context_item(
        &self,
        menu: &NSMenu,
        actions: &mut Vec<ContextAction>,
        title: &str,
        enabled: bool,
        action: ContextAction,
    ) {
        // SAFETY: performContextAction: is defined on AppDelegate and takes the sender.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &NSString::from_str(title),
                Some(sel!(performContextAction:)),
                &NSString::from_str(""),
            )
        };
        // SAFETY: AppDelegate lives for the lifetime of the app.
        unsafe { item.setTarget(Some(&*self.delegate)) };
        item.setTag(actions.len() as isize + 1);
        item.setEnabled(enabled);
        actions.push(action);
        menu.addItem(&item);
    }
}
