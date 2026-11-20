//! Renaming a tab. A double click on a tab in the tab bar puts a text field
//! over its name: Return keeps what was typed, Escape leaves the name as it
//! was, and an empty name gives the tab back the title its panes set.
//!
//! The tab bar is AppKit's own, takes every click on it for itself and has
//! no public way to its tabs. So the window sees its events first (see
//! TabWindow), and the tab under a double click is found by looking through
//! the window's frame for the views AppKit draws tabs with. The field goes
//! into AppKit's view for the tab, whose own label is made invisible
//! meanwhile, and clicks in the field are handed to it from here. (The
//! window's frame cannot hold the field: on macOS 26 a view added there
//! makes AppKit drop its title bar, tab bar included.) Where a system lays
//! its tab bar out some other way, no tab is found and a double click does
//! what it always did.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSApplication, NSButton, NSControl, NSControlTextEditingDelegate, NSEvent,
    NSEventModifierFlags, NSEventType, NSFocusRingType, NSFont, NSLineBreakMode, NSTextAlignment,
    NSTextField, NSTextFieldDelegate, NSTextInputClient, NSTextView,
    NSUserInterfaceLayoutDirection, NSView, NSWindow,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use tern_config::Action;

use super::{app, App};
use crate::mainthread::post_to_main;

/// The classes of AppKit's tab bar and of each tab in it.
const TAB_BAR_CLASS: &[u8] = b"NSTabBar";
const TAB_CLASS: &[u8] = b"NSTabButton";
/// Kept free at each end of a tab, where its close button is.
const FIELD_INSET: f64 = 22.0;
/// A tab narrower than this gives the field all of its width.
const MIN_FIELD_WIDTH: f64 = 60.0;

/// A tab's name being typed.
pub(super) struct Rename {
    /// The window showing the tab bar, which the field is in.
    host: u64,
    /// The window whose tab is being named.
    target: u64,
    /// The title the field started with.
    shown: String,
    field: Retained<NSTextField>,
    delegate: Retained<FieldDelegate>,
    /// AppKit's labels on the tab, invisible while the field shows the
    /// name, and how opaque each was.
    labels: Vec<(Retained<NSTextField>, f64)>,
}

impl App {
    /// A mouse press on its way to a window. True when the press is not
    /// to be passed on: a double click on a tab, which names the tab, or a
    /// press in the name field. A press anywhere else ends naming first,
    /// as a click outside a name does in the Finder.
    pub fn tab_bar_event(&self, window: &NSWindow, event: &NSEvent) -> bool {
        self.tab_bar_event_on(window, window, event)
    }

    /// Handles a mouse press on `event_window` belonging to `window`'s tab bar
    /// (`event_window` is `window` in normal mode, or AppKit's detached
    /// `NSToolbarFullScreenWindow` in full screen).
    pub fn tab_bar_event_on(
        &self,
        window: &NSWindow,
        event_window: &NSWindow,
        event: &NSEvent,
    ) -> bool {
        let at = event.locationInWindow();
        // The field, when it is in `event_window` and the press is inside it.
        let field = self.renaming.borrow().as_ref().map(|r| {
            Some(r.field.clone()).filter(|field| {
                field
                    .window()
                    .is_some_and(|fw| std::ptr::eq(&*fw, event_window))
                    && contains(field.convertRect_toView(field.bounds(), None), at)
            })
        });
        match field {
            Some(Some(field)) => {
                // The tab bar would take the click for the tab under the
                // field. The text gets it instead, to put the insertion
                // point there or to select.
                if event.r#type() == NSEventType::LeftMouseDown
                    && let Some(editor) = field.currentEditor()
                {
                    editor.mouseDown(event);
                }
                return true;
            }
            Some(None) => self.finish_rename(true),
            None => {}
        }
        let is_context = event.r#type() == NSEventType::RightMouseDown
            || (event.r#type() == NSEventType::LeftMouseDown
                && event
                    .modifierFlags()
                    .contains(NSEventModifierFlags::Control));
        if is_context {
            if let Some(target) = self.tab_under(window, at) {
                return self.show_tab_context_menu(window, event_window, event, target);
            }
            return false;
        }
        if event.r#type() != NSEventType::LeftMouseDown || event.clickCount() != 2 {
            return false;
        }
        let Some(target) = self.tab_under(window, at) else {
            return false;
        };
        // On the next turn of the run loop, when AppKit is done with the
        // click that came before this one.
        post_to_main(false, move |_| {
            app().begin_rename(target);
        });
        true
    }

    /// Finds the selected `TabWindow` whose detached full-screen titlebar
    /// window is `bar_win`.
    pub fn owner_of_bar_window(&self, bar_win: &NSWindow) -> Option<Retained<NSWindow>> {
        for (_, window, _) in self.all_windows() {
            let bw = crate::window::bar_window(&window);
            if !std::ptr::eq(&*bw, &*window) && std::ptr::eq(&*bw, bar_win) {
                let selected = window
                    .tabGroup()
                    .and_then(|g| g.selectedWindow())
                    .filter(|w| self.id_of(w).is_some())
                    .unwrap_or(window);
                return Some(selected);
            }
        }
        None
    }

    /// The window whose tab is at a point of this window's tab bar.
    pub fn tab_under(&self, window: &NSWindow, at: NSPoint) -> Option<u64> {
        // In windowed mode the tab bar is above `contentView`; in full screen
        // `at` is in `NSToolbarFullScreenWindow` coordinates instead.
        let bar_win = crate::window::bar_window(window);
        if std::ptr::eq(&*bar_win, window)
            && window
                .contentView()
                .is_some_and(|v| contains(v.frame(), at))
        {
            return None;
        }
        let tabs = tabs_of(window)?;
        let index = tabs.iter().position(|t| contains(t.frame, at))?;
        if controls_in(&tabs[index].view)
            .iter()
            .any(|close| contains(*close, at))
        {
            return None;
        }
        let tab = window.tabbedWindows()?.to_vec().get(index).cloned()?;
        self.id_of(&tab)
    }

    /// The middle of the window's nth tab (from 1), in the window's
    /// coordinates, for the automation socket.
    pub fn tab_center(&self, window: &NSWindow, n: usize) -> Option<NSPoint> {
        let tabs = tabs_of(window)?;
        let frame = tabs.get(n.checked_sub(1)?)?.frame;
        Some(NSPoint::new(
            frame.origin.x + frame.size.width / 2.0,
            frame.origin.y + frame.size.height / 2.0,
        ))
    }

    /// Puts the name field over a window's tab and gives it the keyboard.
    /// False when the tab is not in a tab bar.
    pub fn begin_rename(&self, target: u64) -> bool {
        self.finish_rename(true);
        let Some(tab) = self.window(target) else {
            return false;
        };
        // The tab bar is in the window its group shows.
        let host = tab
            .tabGroup()
            .and_then(|group| group.selectedWindow())
            .unwrap_or_else(|| tab.clone());
        let Some(host_id) = self.id_of(&host) else {
            return false;
        };
        let index = host
            .tabbedWindows()
            .and_then(|tabs| tabs.iter().position(|w| std::ptr::eq(&*w, &*tab)));
        let Some(button) = index.and_then(|i| tabs_of(&host)?.into_iter().nth(i)) else {
            return false;
        };
        let shown = tab.title().to_string();
        let mut labels = Vec::new();
        labels_in(&button.view, &mut labels);
        let field = name_field(self.mtm, &shown, &button, labels.first().map(|l| &**l));
        let delegate = FieldDelegate::new(self.mtm);
        // SAFETY: the delegate lives as long as the field, in the Rename.
        unsafe { field.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
        button.view.addSubview(&field);
        let labels = labels
            .into_iter()
            .map(|label| {
                let alpha = label.alphaValue();
                label.setAlphaValue(0.0);
                (label, alpha)
            })
            .collect();
        *self.renaming.borrow_mut() = Some(Rename {
            host: host_id,
            target,
            shown,
            field: field.clone(),
            delegate,
            labels,
        });
        // A text field that takes the keyboard selects all of its text, so
        // typing replaces the old name. In full screen `button.view` lives in
        // `NSToolbarFullScreenWindow`, which must become key and first
        // responder for the field editor.
        let field_win = button.view.window().unwrap_or_else(|| host.clone());
        if !std::ptr::eq(&*field_win, &*host)
            && !field_win.isKeyWindow()
            && NSApplication::sharedApplication(self.mtm).isActive()
        {
            field_win.makeKeyWindow();
        }
        if !field_win.makeFirstResponder(Some(&field)) {
            self.finish_rename(false);
            return false;
        }
        true
    }

    /// Opens the inline rename field for `target`, revealing the tab bar first
    /// if a single-tab window currently hides it.
    pub fn rename_tab_showing_bar(&self, window: &NSWindow, target: u64) {
        if tabs_of(window).is_none() {
            window.toggleTabBar(None);
        }
        post_to_main(false, move |_| {
            app().begin_rename(target);
        });
    }

    pub fn tab_bar_visible(&self, window: &NSWindow) -> bool {
        tabs_of(window).is_some()
    }

    /// Takes the name field away and gives the keyboard back to the
    /// terminal. With commit, a changed name becomes the tab's.
    pub fn finish_rename(&self, commit: bool) {
        let Some(rename) = self.renaming.take() else {
            return;
        };
        let typed = rename.field.stringValue().to_string();
        // SAFETY: clearing the delegate, so the field going reports nothing.
        unsafe { rename.field.setDelegate(None) };
        if let Some(window) = self.window(rename.host) {
            if !window.isKeyWindow() && NSApplication::sharedApplication(self.mtm).isActive() {
                window.makeKeyWindow();
            }
            if let Some(view) = self.view_of(&window) {
                window.makeFirstResponder(Some(&view));
            }
        }
        rename.field.removeFromSuperview();
        for (label, alpha) in &rename.labels {
            label.setAlphaValue(*alpha);
        }
        if commit
            && let (Some(name), Some(state)) = (
                edited_name(&typed, &rename.shown),
                self.state(rename.target),
            ) {
                state.set_name(name);
            }
        // Return and Escape arrive through the field's own methods, which
        // are still on the stack, so the last release waits for the
        // autorelease pool.
        let _ = Retained::autorelease_ptr(rename.field);
        let _ = Retained::autorelease_ptr(rename.delegate);
    }

    /// The window whose tab is being named.
    pub fn renaming(&self) -> Option<u64> {
        self.renaming.borrow().as_ref().map(|r| r.target)
    }

    /// A window is closing: naming ends without a name when the field or
    /// the tab goes with it.
    pub(super) fn rename_window_closing(&self, id: u64) {
        let involved = self
            .renaming
            .borrow()
            .as_ref()
            .is_some_and(|r| r.host == id || r.target == id);
        if involved {
            self.finish_rename(false);
        }
    }

    /// Another window came to the front, or the field's window changed
    /// size and its tabs with it: what was typed so far is the name.
    pub fn rename_displaced(&self, id: u64, by_other_window: bool) {
        let displaced = self
            .renaming
            .borrow()
            .as_ref()
            .is_some_and(|r| (r.host == id) != by_other_window);
        if displaced {
            self.finish_rename(true);
        }
    }

    /// The field lost the keyboard to something else in its window.
    fn rename_lost_keyboard(&self) {
        let idle = self
            .renaming
            .borrow()
            .as_ref()
            .is_some_and(|r| r.field.currentEditor().is_none());
        if idle {
            self.finish_rename(true);
        }
    }

    /// The text view typing goes to while a name is edited in this window.
    fn name_editor(&self, window: &NSWindow) -> Option<Retained<NSTextView>> {
        let own = self.id_of(window)?;
        let renaming = self.renaming.borrow();
        let rename = renaming.as_ref().filter(|r| r.host == own)?;
        rename.field.currentEditor()?.downcast::<NSTextView>().ok()
    }

    /// Whether a name is being edited in the window, and if so whether any
    /// of it is selected; the menu's Copy item asks.
    pub(super) fn name_selection(&self, window: &NSWindow) -> Option<bool> {
        self.name_editor(window)
            .map(|editor| editor.selectedRange().length > 0)
    }

    /// Copy, Paste and Select All work on the name while one is being
    /// edited in the window, not on the terminal under it. True when the
    /// action was one of them and went to the name.
    pub(super) fn rename_edit(&self, action: &Action, window: &NSWindow) -> bool {
        if !matches!(action, Action::Copy | Action::Paste | Action::SelectAll) {
            return false;
        }
        let Some(editor) = self.name_editor(window) else {
            return false;
        };
        match action {
            Action::Copy => {
                let range = editor.selectedRange();
                if range.length > 0 {
                    let text = editor.string().substringWithRange(range);
                    self.clipboard.set_text(&text.to_string());
                }
            }
            Action::Paste => {
                if let Some(text) = self.clipboard.text() {
                    let line = NSString::from_str(&one_line(&text));
                    // SAFETY: a string, in place of the selection.
                    unsafe { editor.insertText_replacementRange(&line, editor.selectedRange()) };
                }
            }
            // SAFETY: selectAll: takes a sender, which may be nil.
            _ => unsafe { editor.selectAll(None) },
        }
        true
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this type has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TernTabNameDelegate"]
    struct FieldDelegate;

    // SAFETY: NSObjectProtocol has no requirements.
    unsafe impl NSObjectProtocol for FieldDelegate {}

    // SAFETY: the methods below match the protocol's signatures.
    unsafe impl NSControlTextEditingDelegate for FieldDelegate {
        /// Return and Tab keep the name, Escape drops it; every other key
        /// is the field's.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, _control: &NSControl, _text: &NSTextView, command: Sel) -> bool {
            let keeps = [sel!(insertNewline:), sel!(insertTab:), sel!(insertBacktab:)];
            let ends = keeps.contains(&command) || command == sel!(cancelOperation:);
            if ends {
                app().finish_rename(keeps.contains(&command));
            }
            ends
        }

        /// The keyboard went somewhere else. AppKit is in the middle of
        /// moving it, so the field is taken away afterwards.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn did_end_editing(&self, _: &NSNotification) {
            post_to_main(false, |_| app().rename_lost_keyboard());
        }
    }

    // SAFETY: every method of the protocol is optional.
    unsafe impl NSTextFieldDelegate for FieldDelegate {}
);

impl FieldDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<FieldDelegate> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init has this signature.
        unsafe { msg_send![super(this), init] }
    }
}

/// One tab of a tab bar: AppKit's view for it and where that is in the
/// window.
struct Tab {
    view: Retained<NSView>,
    frame: NSRect,
}

/// The view around a window's content and title bar, which the tab bar is
/// somewhere inside. Its coordinates are the window's.
fn frame_view(window: &NSWindow) -> Option<Retained<NSView>> {
    // SAFETY: reading the superview on the main thread.
    unsafe { window.contentView()?.superview() }
}

fn tab_bar_view(window: &NSWindow) -> Option<Retained<NSView>> {
    if let Some(fv) = frame_view(window)
        && let Some(bar) = descendant(&fv, TAB_BAR_CLASS)
    {
        return Some(bar);
    }
    for vc in window.titlebarAccessoryViewControllers().iter() {
        if let Some(bar) = descendant(&vc.view(), TAB_BAR_CLASS) {
            return Some(bar);
        }
    }
    let bw = crate::window::bar_window(window);
    if !std::ptr::eq(&*bw, window)
        && let Some(fv) = frame_view(&bw)
    {
        return descendant(&fv, TAB_BAR_CLASS);
    }
    None
}

/// The tabs in the window's tab bar, in the order of its tabs. None when
/// the window shows no tab bar, or one that is not a view per tab.
fn tabs_of(window: &NSWindow) -> Option<Vec<Tab>> {
    let bar = tab_bar_view(window)?;
    let mut views = Vec::new();
    collect(&bar, TAB_CLASS, &mut views);
    let mut tabs: Vec<Tab> = views
        .into_iter()
        .filter(|view| !view.isHiddenOrHasHiddenAncestor())
        .map(|view| Tab {
            frame: view.convertRect_toView(view.bounds(), None),
            view,
        })
        .collect();
    if tabs.len() != window.tabbedWindows()?.len() {
        return None;
    }
    tabs.sort_by(|a, b| a.frame.origin.x.total_cmp(&b.frame.origin.x));
    let ns = NSApplication::sharedApplication(window.mtm());
    if ns.userInterfaceLayoutDirection() == NSUserInterfaceLayoutDirection::RightToLeft {
        tabs.reverse();
    }
    Some(tabs)
}

fn is_class(view: &NSView, name: &[u8]) -> bool {
    let object: &AnyObject = view;
    object.class().name().to_bytes() == name
}

/// The first view of a class under a view, the view itself included.
fn descendant(view: &NSView, class: &[u8]) -> Option<Retained<NSView>> {
    if is_class(view, class) {
        return Some(view.retain());
    }
    view.subviews()
        .iter()
        .find_map(|child| descendant(&child, class))
}

/// Every view of a class under a view.
fn collect(view: &NSView, class: &[u8], found: &mut Vec<Retained<NSView>>) {
    for child in view.subviews().iter() {
        if is_class(&child, class) {
            found.push(child);
        } else {
            collect(&child, class, found);
        }
    }
}

/// Where the buttons inside a tab are (its close button), in the window.
fn controls_in(view: &NSView) -> Vec<NSRect> {
    let mut found = Vec::new();
    for child in view.subviews().iter() {
        if child.downcast_ref::<NSButton>().is_some() {
            found.push(child.convertRect_toView(child.bounds(), None));
        } else {
            found.extend(controls_in(&child));
        }
    }
    found
}

/// The labels AppKit draws a tab's name with.
fn labels_in(view: &NSView, found: &mut Vec<Retained<NSTextField>>) {
    for child in view.subviews().iter() {
        match child.downcast::<NSTextField>() {
            Ok(label) => found.push(label),
            Err(child) => labels_in(&child, found),
        }
    }
}

/// The field a name is typed into: the tab's label made editable, in
/// place over it. Not yet in the tab's view.
fn name_field(
    mtm: MainThreadMarker,
    text: &str,
    tab: &Tab,
    label: Option<&NSTextField>,
) -> Retained<NSTextField> {
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), NSRect::ZERO);
    field.setStringValue(&NSString::from_str(text));
    let font = label
        .and_then(|label| label.font())
        .unwrap_or_else(|| NSFont::systemFontOfSize(NSFont::smallSystemFontSize()));
    field.setFont(Some(&font));
    if let Some(color) = label.and_then(|label| label.textColor()) {
        field.setTextColor(Some(&color));
    }
    field.setAlignment(NSTextAlignment::Center);
    field.setBezeled(false);
    field.setBordered(false);
    field.setDrawsBackground(false);
    field.setFocusRingType(NSFocusRingType::None);
    field.setEditable(true);
    field.setSelectable(true);
    // One line, which scrolls sideways when the name is longer than the tab.
    field.setUsesSingleLineMode(true);
    field.setLineBreakMode(NSLineBreakMode::ByClipping);
    if let Some(cell) = field.cell() {
        cell.setWraps(false);
        cell.setScrollable(true);
    }
    field.sizeToFit();
    let frame = field_frame(tab.frame, field.frame().size.height);
    field.setFrame(tab.view.convertRect_fromView(frame, None));
    field
}

/// Where the field goes over a tab: as high as its text, in the middle of
/// the tab, and clear of the close button at the tab's end.
fn field_frame(tab: NSRect, height: f64) -> NSRect {
    let inset = FIELD_INSET
        .min((tab.size.width - MIN_FIELD_WIDTH) / 2.0)
        .max(0.0);
    let height = height.min(tab.size.height);
    NSRect::new(
        NSPoint::new(
            (tab.origin.x + inset).round(),
            (tab.origin.y + (tab.size.height - height) / 2.0).round(),
        ),
        NSSize::new((tab.size.width - 2.0 * inset).round(), height),
    )
}

fn contains(rect: NSRect, p: NSPoint) -> bool {
    p.x >= rect.origin.x
        && p.x < rect.origin.x + rect.size.width
        && p.y >= rect.origin.y
        && p.y < rect.origin.y + rect.size.height
}

/// Text as one line: a tab's name has no line breaks or control characters.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// What a finished edit does to the tab's name. None: nothing, the text is
/// what the tab already showed. Some(None): the name is taken away and the
/// tab is titled by its panes again. Some(Some(name)): the tab is named.
fn edited_name(typed: &str, shown: &str) -> Option<Option<String>> {
    let name = one_line(typed).trim().to_string();
    if name == one_line(shown).trim() {
        return None;
    }
    Some((!name.is_empty()).then_some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_names_the_tab_clears_the_name_or_changes_nothing() {
        assert_eq!(edited_name("build", "~"), Some(Some("build".into())));
        assert_eq!(
            edited_name("  my logs \n", "~"),
            Some(Some("my logs".into()))
        );
        assert_eq!(edited_name("a\tb\u{7}c", "~"), Some(Some("a b c".into())));
        // Return on an untouched name does not pin the title the tab had.
        assert_eq!(edited_name("~", "~"), None);
        assert_eq!(edited_name(" ~ ", "~"), None);
        // An empty name goes back to the title the panes set.
        assert_eq!(edited_name("", "build"), Some(None));
        assert_eq!(edited_name("   ", "build"), Some(None));
    }

    #[test]
    fn the_field_sits_in_the_middle_of_its_tab() {
        let tab = NSRect::new(NSPoint::new(204.0, 274.0), NSSize::new(193.0, 24.0));
        let field = field_frame(tab, 14.0);
        assert_eq!(field.origin, NSPoint::new(226.0, 279.0));
        assert_eq!(field.size, NSSize::new(149.0, 14.0));
        // A narrow tab gives up the room kept for the close button.
        let narrow = NSRect::new(NSPoint::new(10.0, 0.0), NSSize::new(70.0, 24.0));
        let field = field_frame(narrow, 14.0);
        assert_eq!((field.origin.x, field.size.width), (15.0, 60.0));
        let tiny = NSRect::new(NSPoint::new(10.0, 0.0), NSSize::new(40.0, 24.0));
        let field = field_frame(tiny, 30.0);
        assert_eq!((field.origin.x, field.size.width), (10.0, 40.0));
        assert_eq!((field.origin.y, field.size.height), (0.0, 24.0));
    }

    #[test]
    fn points_are_in_a_rectangle_up_to_its_far_edges() {
        let r = NSRect::new(NSPoint::new(10.0, 20.0), NSSize::new(30.0, 5.0));
        assert!(contains(r, NSPoint::new(10.0, 20.0)));
        assert!(contains(r, NSPoint::new(39.9, 24.9)));
        assert!(!contains(r, NSPoint::new(40.0, 22.0)));
        assert!(!contains(r, NSPoint::new(20.0, 25.0)));
        assert!(!contains(r, NSPoint::new(9.9, 22.0)));
    }
}
