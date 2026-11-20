//! The NSView that shows a window's panes. It is backed by a CAMetalLayer
//! and forwards every event to the window's state.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSCursor, NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSEvent,
    NSEventModifierFlags, NSEventType, NSResponder, NSTextInputClient, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSViewLayerContentsRedrawPolicy,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSObjectProtocol, NSPoint,
    NSRange, NSRangePointer, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSUInteger,
};
use objc2_quartz_core::{CADisplayLink, CALayer};
use tern_types::Point;

use crate::app::app;
use crate::clipboard::{drop_types, read_drop};
use crate::input::{KeyInput, MouseInput, MouseKind, ScrollInput};
use crate::keymap;
use crate::state::{CursorStyle, WindowState};

pub struct ViewIvars {
    state: Rc<WindowState>,
    /// The input method's uncommitted text.
    marked: RefCell<String>,
    /// Collects what interpretKeyEvents: produces for the key being handled.
    capture: RefCell<Option<Capture>>,
}

#[derive(Default)]
struct Capture {
    text: String,
    commands: Vec<String>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements and this type has no Drop.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TernView"]
    #[ivars = ViewIvars]
    pub struct TernView;

    // SAFETY: each method matches the signature of the NSView or NSResponder
    // method it overrides.
    impl TernView {
        #[unsafe(method_id(makeBackingLayer))]
        fn make_backing_layer(&self) -> Retained<CALayer> {
            Retained::into_super(self.ivars().state.layer().retain())
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        /// AppKit wants the layer's contents redone (during live resize, or
        /// when the layer comes back on screen).
        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            self.ivars().state.mark_dirty();
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {
            if self.window().is_none() {
                return;
            }
            // A display link from the view follows the display the window
            // is on, so the frame rate is right on every screen (macOS 14+).
            // SAFETY: displayLinkFired: is defined below with the signature
            // the display link calls.
            let link = unsafe { self.displayLinkWithTarget_selector(self, sel!(displayLinkFired:)) };
            // SAFETY: the main run loop on the main thread. Common modes keep
            // frames coming during live resize and menu tracking.
            unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
            self.ivars().state.attach_display_link(link);
            self.sync_geometry();
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            self.sync_geometry();
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: NSView's setFrameSize: takes an NSSize.
            let () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.sync_geometry();
        }

        #[unsafe(method(displayLinkFired:))]
        fn display_link_fired(&self, _link: &CADisplayLink) {
            self.ivars().state.tick();
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &cursor(self.ivars().state.cursor()));
        }

        // ---- keyboard ----

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.handle_key_down(event);
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            let input = key_input(event, false, Capture::default(), false);
            self.ivars().state.key_input(input);
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            let flags = event.modifierFlags().0;
            if let Some((key, down)) = keymap::modifier_change(event.keyCode(), flags) {
                let mods = keymap::mods_from_flags(flags);
                self.ivars().state.modifier_changed(key, down, mods, flags);
            }
        }

        #[unsafe(method(viewWillStartLiveResize))]
        fn view_will_start_live_resize(&self) {
            // SAFETY: NSView's method takes no arguments.
            let () = unsafe { msg_send![super(self), viewWillStartLiveResize] };
            self.ivars().state.set_live_resize(true);
        }

        #[unsafe(method(viewDidEndLiveResize))]
        fn view_did_end_live_resize(&self) {
            // SAFETY: NSView's method takes no arguments.
            let () = unsafe { msg_send![super(self), viewDidEndLiveResize] };
            self.ivars().state.set_live_resize(false);
        }

        /// AppKit offers key presses here before the menu and keyDown:.
        /// Bound keys perform their action. Unbound Control keys go straight
        /// to the terminal, because AppKit would otherwise take Ctrl+Tab and
        /// Ctrl+Return for itself and they would never reach keyDown:. The
        /// menu hands keys it has no item for back to keyDown:.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            self.key_equivalent(event)
        }

        // ---- mouse ----

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Down);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Up);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Drag);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.discard_composition();
            let at = self.event_point(event);
            let mods = keymap::mods_from_flags(event.modifierFlags().0);
            if !app().records_input()
                && let Some(ctx) = self.ivars().state.pane_context_at(at, mods)
            {
                app().show_pane_context_menu(self, event, self.ivars().state.id, ctx);
                return;
            }
            self.mouse(event, MouseKind::Down);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Up);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Drag);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Down);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Up);
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Drag);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Move);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let input = ScrollInput {
                at: self.event_point(event),
                dx: event.scrollingDeltaX(),
                dy: event.scrollingDeltaY(),
                precise: event.hasPreciseScrollingDeltas(),
                phase: event.phase().0,
                momentum: event.momentumPhase().0,
                mods: keymap::mods_from_flags(event.modifierFlags().0),
            };
            self.ivars().state.scroll_input(input);
        }
    }

    // SAFETY: NSObjectProtocol has no requirements.
    unsafe impl NSObjectProtocol for TernView {}

    // SAFETY: the methods below match the protocol's signatures.
    unsafe impl NSTextInputClient for TernView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text(&self, string: &AnyObject, _range: NSRange) {
            let text = plain_text(string);
            let had_marked = !self.ivars().marked.take().is_empty();
            if had_marked {
                self.ivars().state.preedit(String::new());
            }
            let mut capture = self.ivars().capture.borrow_mut();
            match capture.as_mut() {
                Some(c) => c.text.push_str(&text),
                None => {
                    drop(capture);
                    self.ivars().state.insert_text(text);
                }
            }
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            // Not passed on to NSResponder, which would beep: the terminal
            // decides what these keys mean.
            if let Some(c) = self.ivars().capture.borrow_mut().as_mut() {
                c.commands.push(selector.name().to_string_lossy().into_owned());
            }
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, string: &AnyObject, _selected: NSRange, _replacement: NSRange) {
            let text = plain_text(string);
            *self.ivars().marked.borrow_mut() = text.clone();
            self.ivars().state.preedit(text);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            if !self.ivars().marked.take().is_empty() {
                self.ivars().state.preedit(String::new());
            }
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(NSNotFound as NSUInteger, 0)
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let marked = self.ivars().marked.borrow();
            if marked.is_empty() {
                NSRange::new(NSNotFound as NSUInteger, 0)
            } else {
                NSRange::new(0, marked.encode_utf16().count())
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(
            &self,
            _range: NSRange,
            _actual: NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            // A terminal has no text document to hand out.
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect_for_character_range(&self, range: NSRange, actual: NSRangePointer) -> NSRect {
            if !actual.is_null() {
                // SAFETY: AppKit passes either null or a valid NSRange to fill.
                unsafe { *actual = range };
            }
            self.ime_screen_rect()
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> NSUInteger {
            NSNotFound as NSUInteger
        }
    }

    // SAFETY: the methods below match the protocol's signatures.
    unsafe impl NSDraggingDestination for TernView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            self.drag_moved(info)
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            self.drag_moved(info)
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _info: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
            self.ivars().state.drag_over(None);
        }

        /// Every drag ends here, dropped or not, so no highlight outlives it.
        #[unsafe(method(draggingEnded:))]
        fn dragging_ended(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) {
            self.ivars().state.drag_moved(None);
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let at = self.to_px(self.convertPoint_fromView(info.draggingLocation(), None));
            match read_drop(&info.draggingPasteboard()) {
                Some(payload) => {
                    self.ivars().state.handle_drop(at, payload);
                    true
                }
                None => false,
            }
        }
    }
);

impl TernView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, state: Rc<WindowState>) -> Retained<TernView> {
        let ivars = ViewIvars {
            state,
            marked: RefCell::default(),
            capture: RefCell::default(),
        };
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: initWithFrame: is NSView's designated initializer.
        let view: Retained<TernView> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setWantsLayer(true);
        view.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::DuringViewResize);

        view.registerForDraggedTypes(&drop_types());

        // mouseMoved: only arrives through a tracking area. InVisibleRect
        // keeps it matching the view as it resizes.
        let options = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the owner is the view, which outlives its own tracking area.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(&view),
                None,
            )
        };
        view.addTrackingArea(&area);
        view
    }

    pub fn state(&self) -> &Rc<WindowState> {
        &self.ivars().state
    }

    /// Reads the view's size and backing scale into the window state.
    pub fn sync_geometry(&self) {
        let size = self.bounds().size;
        let scale = self.window().map_or(1.0, |w| w.backingScaleFactor());
        self.ivars()
            .state
            .set_geometry((size.width, size.height), scale);
    }

    /// Ends a composition without sending it, as when the user clicks or
    /// another pane takes the focus: the text was meant for where it was
    /// being composed, and must not move to another pane.
    pub fn discard_composition(&self) {
        if self.ivars().marked.take().is_empty() {
            return;
        }
        if let Some(context) = self.inputContext() {
            context.discardMarkedText();
        }
        self.ivars().state.preedit(String::new());
    }

    /// Where input method windows go, in screen coordinates.
    pub fn ime_screen_rect(&self) -> NSRect {
        let r = self.ivars().state.ime_cursor_rect();
        let s = self.ivars().state.scale();
        let in_view = NSRect::new(
            NSPoint::new(f64::from(r.x) / s, f64::from(r.y) / s),
            NSSize::new(f64::from(r.w) / s, f64::from(r.h) / s),
        );
        let in_window = self.convertRect_toView(in_view, None);
        self.window()
            .map_or(in_window, |w| w.convertRectToScreen(in_window))
    }

    /// A point in view points (top-left origin) to view pixels.
    pub fn to_px(&self, p: NSPoint) -> Point {
        let s = self.ivars().state.scale();
        Point {
            x: (p.x * s).floor() as i32,
            y: (p.y * s).floor() as i32,
        }
    }

    fn is_first_responder(&self) -> bool {
        let Some(responder) = self.window().and_then(|w| w.firstResponder()) else {
            return false;
        };
        std::ptr::eq(
            Retained::as_ptr(&responder).cast::<AnyObject>(),
            (self as *const Self).cast(),
        )
    }

    fn key_equivalent(&self, event: &NSEvent) -> bool {
        if event.r#type() != NSEventType::KeyDown || !self.is_first_responder() {
            return false;
        }
        if self.ivars().state.escape_cancels(event.keyCode()) {
            return true;
        }
        // A bound key acts here, before the menu sees it, so the bindings
        // decide even where a menu item has the same shortcut. While an
        // input method is composing, keys are its own.
        let state = &self.ivars().state;
        if self.ivars().marked.borrow().is_empty() {
            let input = key_input(event, true, Capture::default(), false);
            if let Some(action) = input.press.as_ref().and_then(|p| state.bound_action(p)) {
                state.key_bound(input, action);
                return true;
            }
        }
        let flags = event.modifierFlags();
        if flags.contains(NSEventModifierFlags::Control)
            && !flags.contains(NSEventModifierFlags::Command)
        {
            self.handle_key_down(event);
            return true;
        }
        // Unbound Command keys go on to the menu, then to keyDown:.
        false
    }

    fn handle_key_down(&self, event: &NSEvent) {
        let ivars = self.ivars();
        let state = &ivars.state;
        let had_marked = !ivars.marked.borrow().is_empty();
        let flags = event.modifierFlags().0;
        if state.escape_cancels(event.keyCode()) {
            return;
        }
        if !had_marked {
            // Bindings come first, so a bound key never reaches an input
            // method or the terminal.
            let input = key_input(event, true, Capture::default(), false);
            if let Some(action) = input.press.as_ref().and_then(|p| state.bound_action(p)) {
                state.key_bound(input, action);
                return;
            }
            if state.option_is_alt(flags) {
                // Option is Alt here: Option-E must reach the program as
                // Alt-E, not start an accent in the input method.
                state.key_input(input);
                return;
            }
        }
        *ivars.capture.borrow_mut() = Some(Capture::default());
        // The input method sees the key first and answers through the
        // NSTextInputClient methods, which fill the capture.
        self.interpretKeyEvents(&NSArray::from_slice(&[event]));
        let capture = ivars.capture.take().unwrap_or_default();
        let composing = had_marked || !ivars.marked.borrow().is_empty();
        state.key_input(key_input(event, true, capture, composing));
    }

    fn mouse(&self, event: &NSEvent, kind: MouseKind) {
        let button = match event.r#type() {
            NSEventType::LeftMouseDown
            | NSEventType::LeftMouseUp
            | NSEventType::LeftMouseDragged => 0,
            NSEventType::RightMouseDown
            | NSEventType::RightMouseUp
            | NSEventType::RightMouseDragged => 1,
            NSEventType::OtherMouseDown
            | NSEventType::OtherMouseUp
            | NSEventType::OtherMouseDragged => event.buttonNumber().clamp(2, 255) as u8,
            _ => 0,
        };
        let clicks = match kind {
            MouseKind::Down | MouseKind::Up => event.clickCount().max(0) as u32,
            MouseKind::Drag | MouseKind::Move => 0,
        };
        if kind == MouseKind::Down {
            self.discard_composition();
        }
        let input = MouseInput {
            kind,
            button,
            at: self.event_point(event),
            mods: keymap::mods_from_flags(event.modifierFlags().0),
            clicks,
        };
        self.ivars().state.mouse(input);
    }

    /// The event's position in view pixels. Events that did not come from
    /// this window (synthesized ones, or ones forwarded from elsewhere) are
    /// converted through screen coordinates.
    fn event_point(&self, event: &NSEvent) -> Point {
        let mut p = event.locationInWindow();
        if let Some(mine) = self.window() {
            match event.window(self.mtm()) {
                Some(theirs) if Retained::as_ptr(&theirs) == Retained::as_ptr(&mine) => {}
                Some(theirs) => p = mine.convertPointFromScreen(theirs.convertPointToScreen(p)),
                None => p = mine.convertPointFromScreen(p),
            }
        }
        self.to_px(self.convertPoint_fromView(p, None))
    }

    fn drag_moved(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
        let at = self.to_px(self.convertPoint_fromView(info.draggingLocation(), None));
        self.ivars().state.drag_over(Some(at));
        NSDragOperation::Copy
    }
}

fn key_input(event: &NSEvent, down: bool, capture: Capture, composing: bool) -> KeyInput {
    let flags = event.modifierFlags().0;
    let code = event.keyCode();
    let string = |s: Option<Retained<NSString>>| s.map(|s| s.to_string()).unwrap_or_default();
    let characters = string(event.characters());
    let ignoring = string(event.charactersIgnoringModifiers());
    // What the key types with no modifiers at all, by the current layout;
    // charactersIgnoringModifiers still applies Shift.
    let base = event
        .charactersByApplyingModifiers(NSEventModifierFlags::empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| ignoring.clone());
    let shifted = event
        .charactersByApplyingModifiers(NSEventModifierFlags::Shift)
        .and_then(|s| single_char(&s.to_string()));
    KeyInput {
        down,
        code,
        flags,
        press: keymap::translate(code, flags, &base),
        characters,
        characters_ignoring_modifiers: ignoring,
        shifted,
        repeat: event.isARepeat(),
        text: capture.text,
        commands: capture.commands,
        composing,
        time: event.timestamp(),
    }
}

/// The string's only character, if it has exactly one.
fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// insertText: and setMarkedText: take an NSString or an NSAttributedString.
fn plain_text(object: &AnyObject) -> String {
    if let Some(s) = object.downcast_ref::<NSString>() {
        s.to_string()
    } else if let Some(s) = object.downcast_ref::<NSAttributedString>() {
        s.string().to_string()
    } else {
        String::new()
    }
}

fn cursor(style: CursorStyle) -> Retained<NSCursor> {
    // The column and row resize cursors that replace these need macOS 15;
    // Tern supports 14.
    #[allow(deprecated)]
    match style {
        CursorStyle::Text => NSCursor::IBeamCursor(),
        CursorStyle::Arrow => NSCursor::arrowCursor(),
        CursorStyle::Pointer => NSCursor::pointingHandCursor(),
        CursorStyle::ColumnResize => NSCursor::resizeLeftRightCursor(),
        CursorStyle::RowResize => NSCursor::resizeUpDownCursor(),
        CursorStyle::Grab => NSCursor::closedHandCursor(),
    }
}
