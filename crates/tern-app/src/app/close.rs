//! Closing panes, tabs and windows, and quitting: when confirm-close is on
//! and a program other than a shell at its prompt is running in what would
//! close, a sheet on the window asks first.

use std::cell::Cell;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSAlertStyle, NSApplication,
    NSApplicationTerminateReply, NSModalResponse, NSWindow,
};
use objc2_foundation::NSString;
use tern_types::PaneId;

use super::App;
use crate::identity;
use crate::mainthread::post_to_main;
use crate::state::WindowState;

/// What the user asked to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseTarget {
    /// A pane of a window (a tab); the tab goes with its last pane.
    Pane(u64, PaneId),
    /// One tab.
    Tab(u64),
    /// Every tab of a window.
    Window(u64),
}

impl App {
    /// Closes what the user asked to close, asking first if something is
    /// running in it.
    pub fn request_close(&self, target: CloseTarget) {
        if self.may_close(target) {
            self.close_now(target);
        }
    }

    /// True when the target may close without asking. Otherwise puts the
    /// question on its window, which closes it if the answer is yes, and
    /// returns false.
    pub fn may_close(&self, target: CloseTarget) -> bool {
        let id = match target {
            CloseTarget::Pane(id, _) | CloseTarget::Tab(id) | CloseTarget::Window(id) => id,
        };
        if self.window(id).is_none() {
            return true;
        }
        let busy = self.busy(target);
        if !self.config().confirm_close || busy.is_empty() {
            return true;
        }
        let what = match target {
            CloseTarget::Pane(..) => "pane",
            CloseTarget::Tab(_) => "tab",
            CloseTarget::Window(_) => "window",
        };
        self.ask(Question {
            window: Some(id),
            subject: Subject::Close(target),
            message: format!("Close this {what}?"),
            detail: format!(
                "{} {} still running in it. Closing the {what} ends {}.",
                list(&busy),
                if busy.len() == 1 { "is" } else { "are" },
                if busy.len() == 1 { "it" } else { "them" },
            ),
            action: "Close",
            cancel: true,
            shown_on: None,
        });
        false
    }

    /// The app is asked to quit (the menu, the key, the automation socket).
    /// When it must ask first, quitting is cancelled and the question put;
    /// a yes quits again, this time without asking. (Answering later with
    /// NSTerminateLater would hold the main thread in a modal run loop mode
    /// where work from other threads never runs.)
    pub(super) fn should_terminate(&self) -> NSApplicationTerminateReply {
        if self.quit_confirmed.get() {
            return NSApplicationTerminateReply::TerminateNow;
        }
        let busy: Vec<String> = self
            .all_windows()
            .iter()
            .flat_map(|(state, _, _)| busy_in(state))
            .collect();
        let busy = dedup(busy);
        if !self.config().confirm_close || busy.is_empty() || self.current_window().is_none() {
            return NSApplicationTerminateReply::TerminateNow;
        }
        self.ask(Question {
            window: None,
            subject: Subject::Quit,
            message: format!("Quit {}?", identity::app_name()),
            detail: format!(
                "{} {} still running. Quitting ends {}.",
                list(&busy),
                if busy.len() == 1 { "is" } else { "are" },
                if busy.len() == 1 { "it" } else { "them" },
            ),
            action: "Quit",
            cancel: true,
            shown_on: None,
        });
        NSApplicationTerminateReply::TerminateCancel
    }

    /// Quits, from the menu, a key or the automation socket, asking first if
    /// something is running. AppKit ignores terminate: while any window
    /// shows a sheet, so the question is put from here, and a question
    /// still showing when the answer is to quit is dismissed first.
    pub fn quit(&self) {
        if self.should_terminate() == NSApplicationTerminateReply::TerminateCancel {
            return;
        }
        self.quit_confirmed.set(true);
        self.questions.borrow_mut().clear();
        for (_, window, _) in self.all_windows() {
            if let Some(sheet) = window.attachedSheet() {
                window.endSheet_returnCode(&sheet, NSAlertSecondButtonReturn);
            }
        }
        // After the sheets have gone.
        post_to_main(true, |mtm| {
            NSApplication::sharedApplication(mtm).terminate(None)
        });
    }

    /// The programs running in what the target would close.
    fn busy(&self, target: CloseTarget) -> Vec<String> {
        let names = match target {
            CloseTarget::Pane(id, pane) => self
                .state(id)
                .and_then(|s| {
                    s.with_panes(|panes| {
                        panes
                            .iter()
                            .find(|p| p.id == pane)
                            .and_then(|p| p.session.foreground_process())
                    })
                })
                .into_iter()
                .collect(),
            CloseTarget::Tab(id) => self.state(id).map(|s| busy_in(&s)).unwrap_or_default(),
            CloseTarget::Window(id) => self
                .tabs_of(id)
                .iter()
                .filter_map(|w| self.id_of(w))
                .filter_map(|id| self.state(id))
                .flat_map(|s| busy_in(&s))
                .collect(),
        };
        dedup(names)
    }

    /// Closes without asking.
    fn close_now(&self, target: CloseTarget) {
        match target {
            CloseTarget::Pane(id, pane) => {
                if let Some(state) = self.state(id) {
                    self.close_pane(&state, pane);
                }
            }
            CloseTarget::Tab(id) => {
                if let Some(window) = self.window(id) {
                    window.close();
                }
            }
            CloseTarget::Window(id) => {
                for tab in self.tabs_of(id) {
                    tab.close();
                }
            }
        }
    }

    /// The window and every tab that shares its tab group, asked of AppKit
    /// each time because tabs move between windows.
    fn tabs_of(&self, id: u64) -> Vec<Retained<NSWindow>> {
        let Some(window) = self.window(id) else {
            return Vec::new();
        };
        let mut tabs: Vec<Retained<NSWindow>> = window
            .tabGroup()
            .map(|g| g.windows().to_vec())
            .unwrap_or_default();
        if !tabs.iter().any(|t| std::ptr::eq(&**t, &*window)) {
            tabs.push(window);
        }
        tabs
    }

    /// Closes a pane, and its tab with its last pane. Nothing asks: a pane
    /// whose shell exited closes this way too.
    pub(super) fn close_pane(&self, state: &WindowState, pane: PaneId) {
        if state.remove_pane(pane)
            && let Some(window) = state.window() {
                window.close();
            }
    }

    /// Puts a question on its window as a sheet. A window already showing
    /// one gets the next when that one is answered; a question already
    /// showing or waiting is not asked twice.
    pub(super) fn ask(&self, question: Question) {
        let asked = self.questions.borrow().iter().any(|q| q.same_as(&question));
        if asked {
            return;
        }
        self.questions.borrow_mut().push(question);
        self.ask_next();
    }

    /// Shows every waiting question whose window has no sheet. A question
    /// about a window that has gone is dropped. One whose sheet went with
    /// its window is put again: a notice on another window, and a quit is
    /// asked afresh, since what runs may have changed. Questions for the
    /// current window wait while there is none.
    pub(super) fn ask_next(&self) {
        let mut i = 0;
        loop {
            let next = self
                .questions
                .borrow()
                .get(i)
                .map(|q| (q.window, q.shown_on, q.subject));
            let Some((id, shown_on, subject)) = next else {
                break;
            };
            if let Some(on) = shown_on {
                if self.window(on).is_some() {
                    i += 1;
                    continue;
                }
                // The sheet closed with its window and will never answer.
                match subject {
                    Subject::Notice(_) => self.questions.borrow_mut()[i].shown_on = None,
                    Subject::Quit => {
                        self.questions.borrow_mut().remove(i);
                        post_to_main(true, |_| super::app().quit());
                        continue;
                    }
                    Subject::Close(_) => {
                        self.questions.borrow_mut().remove(i);
                        continue;
                    }
                }
            }
            let window = match id {
                Some(id) => self.window(id),
                None => self.current_window(),
            };
            let Some(window) = window else {
                if id.is_some() {
                    self.questions.borrow_mut().remove(i);
                } else {
                    i += 1;
                }
                continue;
            };
            let Some(on) = self.id_of(&window) else {
                i += 1;
                continue;
            };
            if window.attachedSheet().is_some() {
                i += 1;
                continue;
            }
            self.questions.borrow_mut()[i].shown_on = Some(on);
            let questions = self.questions.borrow();
            let q = &questions[i];
            let alert = NSAlert::new(self.mtm);
            alert.setAlertStyle(NSAlertStyle::Warning);
            alert.setMessageText(&NSString::from_str(&q.message));
            alert.setInformativeText(&NSString::from_str(&q.detail));
            alert.addButtonWithTitle(&NSString::from_str(q.action));
            if q.cancel {
                alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            }
            let subject = q.subject;
            drop(questions);
            let answered = Cell::new(false);
            let handler = RcBlock::new(move |response: NSModalResponse| {
                if !answered.replace(true) {
                    super::app().answered(subject, response == NSAlertFirstButtonReturn);
                }
            });
            alert.beginSheetModalForWindow_completionHandler(&window, Some(&handler));
            i += 1;
        }
    }

    /// The questions showing or waiting, as (message, showing), for the
    /// automation socket.
    pub fn pending_questions(&self) -> Vec<(String, bool)> {
        self.questions
            .borrow()
            .iter()
            .map(|q| (q.message.clone(), q.shown_on.is_some()))
            .collect()
    }

    /// A question was answered: carry it out, then put up the next.
    fn answered(&self, subject: Subject, yes: bool) {
        self.questions.borrow_mut().retain(|q| q.subject != subject);
        match subject {
            Subject::Close(target) if yes => self.close_now(target),
            Subject::Quit if yes => {
                self.quit_confirmed.set(true);
                // After the sheet has gone and whoever answered has heard back.
                post_to_main(true, |mtm| {
                    NSApplication::sharedApplication(mtm).terminate(None)
                });
            }
            Subject::Close(_) | Subject::Quit | Subject::Notice(_) => {}
        }
        // The sheet is still on its way out, so the next one waits a turn.
        post_to_main(true, |_| super::app().ask_next());
    }
}

/// What a question asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Subject {
    Close(CloseTarget),
    Quit,
    /// Something to tell the user, such as problems in the configuration;
    /// numbered so each is answered on its own.
    Notice(u64),
}

/// A question for the user, waiting for its window or on it.
pub(super) struct Question {
    /// The window it goes on; None for the current window.
    pub(super) window: Option<u64>,
    pub(super) subject: Subject,
    pub(super) message: String,
    pub(super) detail: String,
    /// The first button's title.
    pub(super) action: &'static str,
    /// Whether there is a Cancel button too.
    pub(super) cancel: bool,
    /// The window whose sheet shows it; None while it waits.
    pub(super) shown_on: Option<u64>,
}

impl Question {
    /// Whether asking this would repeat a question already showing or
    /// waiting: the same request, or a notice saying the same thing (a
    /// faulty configuration reloaded again).
    fn same_as(&self, other: &Question) -> bool {
        match (self.subject, other.subject) {
            (Subject::Notice(_), Subject::Notice(_)) => {
                self.message == other.message && self.detail == other.detail
            }
            (a, b) => a == b,
        }
    }
}

/// The programs running in a tab's panes.
fn busy_in(state: &WindowState) -> Vec<String> {
    state.with_panes(|panes| {
        panes
            .iter()
            .filter_map(|p| p.session.foreground_process())
            .collect()
    })
}

/// Names in their first order, each once.
fn dedup(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// "a", "a and b", "a, b and c".
fn list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("“{one}”"),
        [rest @ .., last] => {
            let rest: Vec<String> = rest.iter().map(|n| format!("“{n}”")).collect();
            format!("{} and “{last}”", rest.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(subject: Subject, detail: &str) -> Question {
        Question {
            window: None,
            subject,
            message: "The configuration has problems".into(),
            detail: detail.into(),
            action: "OK",
            cancel: false,
            shown_on: None,
        }
    }

    #[test]
    fn a_notice_saying_the_same_is_not_queued_twice() {
        let first = question(Subject::Notice(1), "bogus-key: unknown setting");
        assert!(first.same_as(&question(Subject::Notice(2), "bogus-key: unknown setting")));
        assert!(!first.same_as(&question(Subject::Notice(2), "font-size: too large")));
        assert!(question(Subject::Quit, "").same_as(&question(Subject::Quit, "other")));
        assert!(!first.same_as(&question(Subject::Quit, "bogus-key: unknown setting")));
    }

    #[test]
    fn running_programs_are_listed_once_each() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            dedup(names(&["vim", "make", "vim"])),
            names(&["vim", "make"])
        );
        assert_eq!(list(&names(&["vim"])), "“vim”");
        assert_eq!(list(&names(&["vim", "make"])), "“vim” and “make”");
        assert_eq!(
            list(&names(&["vim", "make", "top"])),
            "“vim”, “make” and “top”"
        );
        assert_eq!(list(&[]), "");
    }
}
