//! Windows: creation, the window class and the window delegate. Each
//! NSWindow is one tab.

use std::rc::Rc;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSApplication, NSBackingStoreType, NSColor, NSEvent, NSEventType, NSResponder, NSScreen,
    NSView, NSWindow, NSWindowButton, NSWindowDelegate, NSWindowOcclusionState, NSWindowStyleMask,
    NSWindowTabbingMode,
};
use objc2_foundation::{
    ns_string, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSTimer,
};
use tern_render::CellMetrics;
use tern_types::Rgb;

use crate::app::{app, CloseTarget};
use crate::pane::Pane;
use crate::startup::StartupProbe;
use crate::state::WindowState;
use crate::view::TernView;

/// Windows with this identifier share tab groups.
const TABBING_ID: &str = "com.kaicui.tern.terminal";
const MIN_CONTENT_SIZE: NSSize = NSSize::new(160.0, 100.0);

/// Every window's style: a titled, resizable window.
pub fn window_style() -> NSWindowStyleMask {
    NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
}

/// Returns the window currently hosting `window`'s titlebar and tab bar:
/// `window` itself in normal mode, or AppKit's detached
/// `NSToolbarFullScreenWindow` while in full screen.
pub fn bar_window(window: &NSWindow) -> Retained<NSWindow> {
    if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        let selected = window.tabGroup().and_then(|g| g.selectedWindow());
        let ns = NSApplication::sharedApplication(window.mtm());
        for candidate in ns.windows().iter() {
            let obj: &AnyObject = &candidate;
            if obj.class().name().to_bytes() != b"NSToolbarFullScreenWindow" {
                continue;
            }
            if let Some(parent) = candidate.parentWindow()
                && (std::ptr::eq(&*parent, window)
                    || selected
                        .as_deref()
                        .is_some_and(|s| std::ptr::eq(&*parent, s)))
            {
                return candidate;
            }
        }
    }
    for vc in window.titlebarAccessoryViewControllers().iter() {
        if let Some(w) = vc.view().window()
            && !std::ptr::eq(&*w, window)
        {
            return w;
        }
    }
    if let Some(btn) = window.standardWindowButton(NSWindowButton::CloseButton)
        && let Some(w) = btn.window()
        && !std::ptr::eq(&*w, window)
    {
        return w;
    }
    window.retain()
}

fn enable_fullscreen_transparent_titlebar(window: &NSWindow) {
    let sel_fs = sel!(setTitlebarAppearsTransparentWhenFullscreen:);
    if window.respondsToSelector(sel_fs) {
        // SAFETY: NSWindow method taking BOOL.
        let _: () = unsafe { msg_send![window, setTitlebarAppearsTransparentWhenFullscreen: true] };
    }
}

fn root_view(window: &NSWindow) -> Option<Retained<NSView>> {
    let mut cur = window.contentView()?;
    // SAFETY: reading superview on the main thread.
    while let Some(parent) = unsafe { cur.superview() } {
        cur = parent;
    }
    Some(cur)
}

fn descendant_by_class(view: &NSView, class: &[u8]) -> Option<Retained<NSView>> {
    let obj: &AnyObject = view;
    if obj.class().name().to_bytes() == class {
        return Some(view.retain());
    }
    view.subviews()
        .iter()
        .find_map(|child| descendant_by_class(&child, class))
}

fn for_each_descendant(view: &NSView, class: &[u8], f: &mut impl FnMut(&NSView)) {
    let obj: &AnyObject = view;
    if obj.class().name().to_bytes() == class {
        f(view);
    }
    for child in view.subviews().iter() {
        for_each_descendant(&child, class, f);
    }
}

fn style_titlebar_views(
    host: &NSWindow,
    color: &NSColor,
    appearance: Option<&NSAppearance>,
    dark: bool,
    in_fullscreen_bar: bool,
) {
    let Some(root) = root_view(host) else {
        return;
    };
    let Some(container) = descendant_by_class(&root, b"NSTitlebarContainerView") else {
        return;
    };
    let cg = color.CGColor();
    container.setAppearance(appearance);
    if in_fullscreen_bar {
        container.setWantsLayer(true);
        if let Some(layer) = container.layer() {
            layer.setBackgroundColor(Some(&cg));
        }
    } else if let Some(layer) = container.layer() {
        layer.setBackgroundColor(None);
    }
    if let Some(titlebar) = descendant_by_class(&container, b"NSTitlebarView") {
        titlebar.setWantsLayer(true);
        if let Some(layer) = titlebar.layer() {
            layer.setBackgroundColor(Some(&cg));
        }
    }
    for_each_descendant(&container, b"NSTitlebarBackgroundView", &mut |bg_view| {
        bg_view.setHidden(true);
    });
    for_each_descendant(&container, b"NSVisualEffectView", &mut |vev| {
        vev.setHidden(true);
    });
    if let Some(tab_bar) = descendant_by_class(&container, b"NSTabBar") {
        tab_bar.setAppearance(appearance);
        for_each_descendant(&tab_bar, b"NSGlassEffectView", &mut |glass| {
            glass.setAppearance(appearance);
        });
        if let Some(track) = descendant_by_class(&tab_bar, b"NSSubduedGlassEffectView") {
            track.setAppearance(appearance);
            if let Some(first) = track.subviews().firstObject() {
                first.setAlphaValue(if dark { 0.5 } else { 1.0 });
            }
        }
    }
}

/// Gives a window (and its detached full-screen titlebar window, if any) the
/// terminal's background color and matching light/dark appearance so the title
/// bar, tab bar and terminal read as one surface in both windowed and
/// full-screen modes.
pub fn tint(window: &NSWindow, bg: Rgb) {
    let unit = |shift: u32| f64::from((bg >> shift) & 0xff) / 255.0;
    // sRGB, as the Metal layer is, so the two meet without a seam.
    let color = NSColor::colorWithSRGBRed_green_blue_alpha(unit(16), unit(8), unit(0), 1.0);
    let dark = !is_light(bg);
    window.setBackgroundColor(Some(&color));
    window.setTitlebarAppearsTransparent(true);
    enable_fullscreen_transparent_titlebar(window);
    // SAFETY: reading framework constants.
    let name = unsafe {
        if dark {
            NSAppearanceNameDarkAqua
        } else {
            NSAppearanceNameAqua
        }
    };
    let appearance = NSAppearance::appearanceNamed(name);
    window.setAppearance(appearance.as_deref());
    style_titlebar_views(window, &color, appearance.as_deref(), dark, false);

    let bar = bar_window(window);
    if !std::ptr::eq(&*bar, window) {
        // AppKit's detached window extends below the tab bar, over the
        // first terminal rows. Only tint the titlebar views within it.
        bar.setOpaque(false);
        bar.setBackgroundColor(Some(&NSColor::clearColor()));
        bar.setAppearance(appearance.as_deref());
        if let Some(cv) = bar.contentView() {
            cv.setAppearance(appearance.as_deref());
        }
        style_titlebar_views(&bar, &color, appearance.as_deref(), dark, true);
    }

    let ns = NSApplication::sharedApplication(window.mtm());
    if window.isKeyWindow() || window.isMainWindow() || ns.appearance().is_none() {
        ns.setAppearance(appearance.as_deref());
    }
}

/// Reapplies chrome styling after a short delay because AppKit resets the
/// titlebar and tab bar views asynchronously after full-screen transitions and
/// tab-group changes on macOS 26/27.
pub fn reapply_chrome_soon(id: u64) {
    let block = block2::RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
        if let Some(state) = app().state(id) {
            state.reapply_chrome();
        }
    });
    // SAFETY: one-shot timer scheduled on the main run loop.
    let _ = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.06, false, &block) };
}

/// Whether dark text reads better than light text on this color.
fn is_light(color: Rgb) -> bool {
    let channel = |shift: u32| f64::from((color >> shift) & 0xff);
    0.299 * channel(16) + 0.587 * channel(8) + 0.114 * channel(0) > 127.5
}

/// One window and the objects that serve it. The app's window list owns it.
pub struct TernWindow {
    pub state: Rc<WindowState>,
    pub window: Retained<NSWindow>,
    pub view: Retained<TernView>,
    delegate: Retained<WindowDelegate>,
}

impl TernWindow {
    pub fn new(
        mtm: MainThreadMarker,
        id: u64,
        content: NSRect,
        screen: Option<&NSScreen>,
        pane: Pane,
        metrics: CellMetrics,
        startup: Option<Arc<StartupProbe>>,
    ) -> TernWindow {
        // With a screen, AppKit takes the rectangle relative to that
        // screen's origin, and keeps the window there.
        let mut rect = content;
        if let Some(screen) = screen {
            let origin = screen.frame().origin;
            rect.origin = NSPoint::new(rect.origin.x - origin.x, rect.origin.y - origin.y);
        }
        // SAFETY: NSWindow's designated initializer with its arguments.
        // Release-when-closed is turned off below, so the Retained here is
        // the owning reference.
        let window: Retained<TabWindow> = unsafe {
            msg_send![
                super(TabWindow::alloc(mtm).set_ivars(())),
                initWithContentRect: rect,
                styleMask: window_style(),
                backing: NSBackingStoreType::Buffered,
                defer: false,
                screen: screen,
            ]
        };
        let window: Retained<NSWindow> = Retained::into_super(window);
        // SAFETY: Rust owns the window; AppKit must not release it on close.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setRestorable(false);
        window.setTabbingMode(NSWindowTabbingMode::Preferred);
        window.setTabbingIdentifier(ns_string!(TABBING_ID));
        window.setTitle(ns_string!("Tern"));
        // The window's background shows in the title bar; see tint.
        window.setTitlebarAppearsTransparent(true);
        enable_fullscreen_transparent_titlebar(&window);
        window.setContentMinSize(MIN_CONTENT_SIZE);

        let state = Rc::new(WindowState::new(id, &window, pane, metrics, startup));
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), content.size);
        let view = TernView::new(mtm, bounds, Rc::clone(&state));
        window.setContentView(Some(&view));
        window.makeFirstResponder(Some(&view));
        let delegate = WindowDelegate::new(mtm, Rc::clone(&state));
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        TernWindow {
            state,
            window,
            view,
            delegate,
        }
    }

    /// Lets go of the window once AppKit has finished closing it.
    pub fn retire(self) {
        self.state.shutdown();
        self.window.setDelegate(None);
        // windowWillClose: is still on the stack and AppKit uses the window
        // after it returns, so the last release waits for the autorelease pool.
        let _ = Retained::autorelease_ptr(self.window);
        let _ = Retained::autorelease_ptr(self.view);
        let _ = Retained::autorelease_ptr(self.delegate);
    }
}

define_class!(
    // SAFETY: NSWindow may be subclassed and sendEvent: overridden, and this
    // type has no Drop.
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TernTabWindow"]
    struct TabWindow;

    impl TabWindow {
        /// Every event comes through here before AppKit acts on it. The
        /// tab bar is AppKit's and takes its clicks for itself, so this is
        /// where a double click on a tab is seen in time to name the tab,
        /// and where a drag of a tab is seen to start and to end.
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            let press = matches!(
                event.r#type(),
                NSEventType::LeftMouseDown
                    | NSEventType::RightMouseDown
                    | NSEventType::OtherMouseDown
            );
            if press && app().tab_bar_event(self, event) {
                return;
            }
            // AppKit follows a press on a tab until the mouse is let go,
            // and only then does the press come back from it.
            let on_tab = app().tab_pressed(self, event);
            // SAFETY: NSWindow's sendEvent: takes the event.
            let () = unsafe { msg_send![super(self), sendEvent: event] };
            if on_tab {
                app().tab_released();
            }
        }
    }
);

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this type has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TernWindowDelegate"]
    #[ivars = Rc<WindowState>]
    pub struct WindowDelegate;

    // SAFETY: NSObjectProtocol has no requirements.
    unsafe impl NSObjectProtocol for WindowDelegate {}

    // SAFETY: the methods below match the protocol's signatures.
    unsafe impl NSWindowDelegate for WindowDelegate {
        /// The close button, and Close in the menu AppKit adds: the tab
        /// closes unless a program in it is running and the user says no.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _: &NSWindow) -> bool {
            app().may_close(CloseTarget::Tab(self.ivars().id))
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _: &NSNotification) {
            app().window_will_close(self.ivars().id);
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _: &NSNotification) {
            let state = self.ivars();
            state.reapply_chrome();
            if let Some(w) = state.window()
                && (w.styleMask().contains(NSWindowStyleMask::FullScreen)
                    || w.tabbedWindows().is_some_and(|t| t.len() > 1))
            {
                reapply_chrome_soon(state.id);
            }
            app().window_became_key(state.id);
        }

        #[unsafe(method(windowDidBecomeMain:))]
        fn window_did_become_main(&self, _: &NSNotification) {
            let state = self.ivars();
            state.reapply_chrome();
            reapply_chrome_soon(state.id);
        }

        #[unsafe(method(windowWillEnterFullScreen:))]
        fn window_will_enter_full_screen(&self, _: &NSNotification) {
            app().rename_displaced(self.ivars().id, false);
            self.ivars().reapply_chrome();
        }

        #[unsafe(method(windowDidEnterFullScreen:))]
        fn window_did_enter_full_screen(&self, _: &NSNotification) {
            self.ivars().reapply_chrome();
            reapply_chrome_soon(self.ivars().id);
            self.sync_view();
        }

        #[unsafe(method(windowDidExitFullScreen:))]
        fn window_did_exit_full_screen(&self, _: &NSNotification) {
            self.ivars().reapply_chrome();
            reapply_chrome_soon(self.ivars().id);
            self.sync_view();
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _: &NSNotification) {
            self.ivars().set_focused(false);
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _: &NSNotification) {
            // The tabs moved under a name being typed over one of them.
            let state = self.ivars();
            app().rename_displaced(state.id, false);
            if let Some(w) = state.window()
                && w.styleMask().contains(NSWindowStyleMask::FullScreen)
            {
                state.reapply_chrome();
            }
            self.sync_view();
        }

        #[unsafe(method(windowDidChangeBackingProperties:))]
        fn window_did_change_backing_properties(&self, _: &NSNotification) {
            self.sync_view();
        }

        #[unsafe(method(windowDidChangeOcclusionState:))]
        fn window_did_change_occlusion_state(&self, _: &NSNotification) {
            let state = self.ivars();
            if let Some(window) = state.window() {
                state.set_occluded(!window.occlusionState().contains(NSWindowOcclusionState::Visible));
            }
        }
    }

    impl WindowDelegate {
        /// The tab bar's + button sends this up the window's responder
        /// chain, which includes the window's delegate.
        #[unsafe(method(newWindowForTab:))]
        fn new_window_for_tab(&self, _sender: Option<&AnyObject>) {
            app().new_tab_next_to(self.ivars().id);
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker, state: Rc<WindowState>) -> Retained<WindowDelegate> {
        let this = Self::alloc(mtm).set_ivars(state);
        // SAFETY: NSObject's init has this signature.
        unsafe { msg_send![super(this), init] }
    }

    fn sync_view(&self) {
        if let Some(view) = self.ivars().window().and_then(|w| w.contentView())
            && let Ok(view) = view.downcast::<TernView>() {
                view.sync_geometry();
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark_backgrounds_are_told_apart() {
        for dark in [0x000000, 0x181a1f, 0x203040, 0x002b36, 0x0000ff, 0x7f7f7f] {
            assert!(!is_light(dark), "{dark:06x}");
        }
        for light in [0xffffff, 0xfdf6e3, 0xfafafa, 0xffff00, 0x808080] {
            assert!(is_light(light), "{light:06x}");
        }
    }
}
