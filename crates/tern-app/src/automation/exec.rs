//! Carrying out requests, on the main thread.

use objc2::rc::Retained;
use objc2::{sel, MainThreadMarker};
use objc2_app_kit::{
    NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSApplication, NSEvent,
    NSEventModifierFlags, NSEventType, NSMenu, NSScreen, NSTextInputClient, NSWindow,
    NSWindowOrderingMode,
};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGDataProvider, CGEvent, CGImage, CGRectNull, CGScrollEventUnit, CGWindowImageOption,
    CGWindowListOption,
};
use objc2_foundation::{NSNotFound, NSPoint, NSProcessInfo, NSRange, NSSize, NSString};
use tern_types::PaneId;

use super::{error, json, Command, KeySpec};
use crate::app::app;
use crate::input::MouseKind;
use crate::keymap::flag;
use crate::menu::AppCommand;
use crate::pane::Pane;
use crate::view::TernView;

pub(super) fn execute(mtm: MainThreadMarker, command: Command) -> String {
    let app = app();
    let target = app
        .current_window()
        .and_then(|w| app.view_of(&w).map(|v| (w, v)));
    let before = target.as_ref().map_or(0, |(_, v)| v.state().last_seq());
    let result = match command {
        Command::State => return json::state(mtm),
        Command::Menu => return json::menu(mtm),
        Command::MenuClick(command) => return menu_click(mtm, &command),
        Command::Panes => return json::panes(),
        Command::Stats => return json::stats(),
        Command::StatsReset => {
            for (state, _, _) in app.all_windows() {
                state.clear_frame_times();
                state.clear_key_latency();
            }
            return OK.into();
        }
        Command::Action(command) => {
            app.run_command(command, None);
            return OK.into();
        }
        Command::Clipboard => {
            let text = app.clipboard().text().unwrap_or_default();
            return format!("{{\"ok\":true,\"text\":{}}}", json::string(&text));
        }
        Command::SetClipboard(text) => {
            app.clipboard().set_text(&text);
            return OK.into();
        }
        Command::Screen(pane) => return screen(target.as_ref().map(|(_, v)| v), pane),
        Command::Drawn(pane) => {
            let holder = match pane {
                Some(id) => app
                    .all_windows()
                    .into_iter()
                    .find(|(state, _, _)| state.contains_pane(id))
                    .map(|(state, _, _)| (state, id)),
                None => target.as_ref().and_then(|(_, v)| {
                    let state = std::rc::Rc::clone(v.state());
                    state.focused_pane_id().map(|id| (state, id))
                }),
            };
            return match holder.and_then(|(state, id)| state.drawn_text(id)) {
                Some(text) => format!("{{\"ok\":true,\"text\":{}}}", json::string(&text)),
                None => error("no such pane"),
            };
        }
        Command::Selection(pane) => {
            return pane_text(target.as_ref().map(|(_, v)| v), pane, |p| {
                p.session.selection_text().unwrap_or_default()
            })
        }
        Command::Screenshot(path) => {
            let Some((_, view)) = target else {
                return error("no window");
            };
            return match view.state().render_offscreen() {
                Ok(image) => match image.write_png(&path) {
                    Ok(()) => format!(
                        "{{\"ok\":true,\"width\":{},\"height\":{}}}",
                        image.width, image.height
                    ),
                    Err(e) => error(&format!("{}: {e}", path.display())),
                },
                Err(e) => error(&e.to_string()),
            };
        }
        Command::WindowImage(path) => {
            let Some((window, _)) = target else {
                return error("no window");
            };
            let Some(image) = window_image(&window) else {
                return error("no image of the window");
            };
            return match image.write_png(&path) {
                Ok(()) => format!(
                    "{{\"ok\":true,\"width\":{},\"height\":{}}}",
                    image.width, image.height
                ),
                Err(e) => error(&format!("{}: {e}", path.display())),
            };
        }
        Command::Quit => return OK.into(),
        Command::TabCenter(n) => {
            let Some((window, view)) = target else {
                return error("no window");
            };
            return match app.tab_center(&window, n) {
                Some(at) => {
                    let at = view.convertPoint_fromView(at, None);
                    format!("{{\"ok\":true,\"x\":{},\"y\":{}}}", at.x, at.y)
                }
                None => error("the window shows no such tab"),
            };
        }
        Command::Open(paths) => {
            app.open_paths(paths);
            return OK.into();
        }
        Command::Reopen => {
            app.reopen(false);
            return OK.into();
        }
        Command::Confirm(yes) => return confirm(yes),
        Command::FocusPane(id) => {
            let holder = app
                .all_windows()
                .into_iter()
                .find(|(state, _, _)| state.contains_pane(id));
            return match holder {
                Some((state, _, _)) => {
                    state.focus_pane(id);
                    OK.into()
                }
                None => error("no such pane"),
            };
        }
        _ => {
            let Some((window, view)) = target else {
                return error("no window");
            };
            input(mtm, &window, &view, command).map(|()| view)
        }
    };
    match result {
        Err(e) => error(&e),
        // Report what the request produced in the window it went to.
        Ok(view) => {
            let events: Vec<String> = view
                .state()
                .records_after(before)
                .iter()
                .map(|(seq, r)| json::record(*seq, r))
                .collect();
            format!("{{\"ok\":true,\"events\":[{}]}}", events.join(","))
        }
    }
}

const OK: &str = "{\"ok\":true}";

/// Finds the menu item that performs a command and has its menu perform it,
/// the way choosing it with the mouse does, validation included.
fn menu_click(mtm: MainThreadMarker, command: &AppCommand) -> String {
    let app = app();
    let Some(main) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return error("no main menu");
    };
    for top in main.itemArray().iter() {
        let Some(menu) = top.submenu() else { continue };
        menu.update();
        for (i, item) in menu.itemArray().iter().enumerate() {
            if app.menu_command(item.tag()).as_ref() != Some(command) || item.isSeparatorItem() {
                continue;
            }
            if !item.isEnabled() {
                return error("the menu item is disabled");
            }
            menu.performActionForItemAtIndex(i as isize);
            return OK.into();
        }
    }
    error("no menu item performs that command")
}

/// Answers the question a window shows (to close, or to quit), as clicking
/// its first button (yes) or Cancel (no) does.
fn confirm(yes: bool) -> String {
    let pending = app()
        .all_windows()
        .into_iter()
        .find_map(|(_, window, _)| window.attachedSheet().map(|sheet| (window, sheet)));
    let Some((window, sheet)) = pending else {
        return error("no question is pending");
    };
    let answer = if yes {
        NSAlertFirstButtonReturn
    } else {
        NSAlertSecondButtonReturn
    };
    window.endSheet_returnCode(&sheet, answer);
    OK.into()
}

/// A pane's visible text: the given pane wherever it is, or the current
/// window's focused pane.
fn screen(view: Option<&Retained<TernView>>, pane: Option<PaneId>) -> String {
    pane_text(view, pane, |p| p.session.text())
}

/// Some text about a pane: the given pane wherever it is, or the current
/// window's focused pane.
fn pane_text(
    view: Option<&Retained<TernView>>,
    pane: Option<PaneId>,
    text: impl Fn(&Pane) -> String,
) -> String {
    let text = match pane {
        Some(id) => app()
            .all_windows()
            .iter()
            .find_map(|(state, _, _)| state.with_pane(id, &text)),
        None => view.and_then(|v| v.state().with_focused(&text)),
    };
    match text {
        Some(text) => format!("{{\"ok\":true,\"text\":{}}}", json::string(&text)),
        None => error("no such pane"),
    }
}

fn input(
    mtm: MainThreadMarker,
    window: &NSWindow,
    view: &TernView,
    command: Command,
) -> Result<(), String> {
    let now = NSProcessInfo::processInfo().systemUptime();
    match command {
        Command::Key(spec) => send_key(mtm, window, &spec, now),
        Command::Modifier { code, flags } => {
            // Down with the modifier's flags, then up without them.
            for flags in [flags, 0] {
                let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                    NSEventType::FlagsChanged,
                    NSPoint::new(0.0, 0.0),
                    NSEventModifierFlags(flags),
                    now,
                    window.windowNumber(),
                    None,
                    &NSString::new(),
                    &NSString::new(),
                    false,
                    code,
                )
                .ok_or("AppKit refused to make the modifier event")?;
                window.sendEvent(&event);
            }
            Ok(())
        }
        Command::Text(text) => {
            // SAFETY: an NSString is a valid argument for insertText:.
            unsafe {
                view.insertText_replacementRange(
                    &NSString::from_str(&text),
                    NSRange::new(NSNotFound as usize, 0),
                )
            };
            Ok(())
        }
        Command::Mouse {
            kind,
            x,
            y,
            button,
            flags,
            clicks,
        } => {
            let ty = match (kind, button) {
                (MouseKind::Down, 0) => NSEventType::LeftMouseDown,
                (MouseKind::Down, 1) => NSEventType::RightMouseDown,
                (MouseKind::Down, _) => NSEventType::OtherMouseDown,
                (MouseKind::Up, 0) => NSEventType::LeftMouseUp,
                (MouseKind::Up, 1) => NSEventType::RightMouseUp,
                (MouseKind::Up, _) => NSEventType::OtherMouseUp,
                (MouseKind::Drag, 0) => NSEventType::LeftMouseDragged,
                (MouseKind::Drag, 1) => NSEventType::RightMouseDragged,
                (MouseKind::Drag, _) => NSEventType::OtherMouseDragged,
                (MouseKind::Move, _) => NSEventType::MouseMoved,
            };
            let pressing = matches!(kind, MouseKind::Down | MouseKind::Drag);
            let clicks = if matches!(kind, MouseKind::Down | MouseKind::Up) {
                isize::from(clicks)
            } else {
                0
            };
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                ty,
                view.convertPoint_toView(NSPoint::new(x, y), None),
                NSEventModifierFlags(flags),
                now,
                window.windowNumber(),
                None,
                0,
                clicks,
                if pressing { 1.0 } else { 0.0 },
            )
            .ok_or("AppKit refused to make the mouse event")?;
            // While a pane is picked up in a tab that another tab has since
            // hidden, the drag and the release reach its view through the
            // app's event monitor, as the real ones do.
            if matches!(ty, NSEventType::LeftMouseDragged | NSEventType::LeftMouseUp)
                && app().pane_move_hidden()
            {
                NSApplication::sharedApplication(mtm).sendEvent(&event);
                return Ok(());
            }
            // Straight to the view: NSWindow's sendEvent: would make the
            // window key on a click and could bring the app to the front.
            match ty {
                NSEventType::LeftMouseDown => view.mouseDown(&event),
                NSEventType::RightMouseDown => view.rightMouseDown(&event),
                NSEventType::OtherMouseDown => view.otherMouseDown(&event),
                NSEventType::LeftMouseUp => view.mouseUp(&event),
                NSEventType::RightMouseUp => view.rightMouseUp(&event),
                NSEventType::OtherMouseUp => view.otherMouseUp(&event),
                NSEventType::LeftMouseDragged => view.mouseDragged(&event),
                NSEventType::RightMouseDragged => view.rightMouseDragged(&event),
                NSEventType::OtherMouseDragged => view.otherMouseDragged(&event),
                _ => view.mouseMoved(&event),
            }
            Ok(())
        }
        Command::Scroll {
            x,
            y,
            dx,
            dy,
            lines,
        } => {
            let unit = if lines {
                CGScrollEventUnit::Line
            } else {
                CGScrollEventUnit::Pixel
            };
            let cg = CGEvent::new_scroll_wheel_event2(None, unit, 2, dy, dx, 0)
                .ok_or("Core Graphics refused to make the scroll event")?;
            let on_screen =
                window.convertPointToScreen(view.convertPoint_toView(NSPoint::new(x, y), None));
            // Core Graphics puts the origin at the top left of the primary display.
            let primary_height = NSScreen::screens(mtm)
                .firstObject()
                .map_or(0.0, |s| s.frame().size.height);
            CGEvent::set_location(
                Some(&cg),
                CGPoint::new(on_screen.x, primary_height - on_screen.y),
            );
            let event = NSEvent::eventWithCGEvent(&cg).ok_or("AppKit refused the scroll event")?;
            view.scrollWheel(&event);
            Ok(())
        }
        Command::DragOver { x, y } => {
            // As draggingEntered: and draggingUpdated: report it.
            view.state().drag_over(Some(view.to_px(NSPoint::new(x, y))));
            Ok(())
        }
        Command::DragExit => {
            view.state().drag_over(None);
            Ok(())
        }
        Command::TabMove { window: id, to } => {
            let app = app();
            let moved = app.window(id).ok_or("no such window")?;
            let to = app.window(to).ok_or("no such window to move to")?;
            if to
                .tabGroup()
                .is_some_and(|g| g.windows().iter().any(|w| std::ptr::eq(&*w, &*moved)))
            {
                return Err("the tab is already in that window".into());
            }
            // What dropping a dragged tab on another window's tab bar does.
            if let Some(group) = moved.tabGroup() {
                group.removeWindow(&moved);
            }
            to.addTabbedWindow_ordered(&moved, NSWindowOrderingMode::Above);
            if let Some(group) = moved.tabGroup() {
                group.setSelectedWindow(Some(&moved));
            }
            crate::app::keep_in_front(&moved);
            Ok(())
        }
        Command::Drop { x, y, payload } => {
            // A drag session cannot be synthesized; this is the call
            // performDragOperation: makes.
            view.state()
                .handle_drop(view.to_px(NSPoint::new(x, y)), payload);
            Ok(())
        }
        Command::Resize { w, h, live } => {
            if live {
                view.viewWillStartLiveResize();
            }
            window.setContentSize(NSSize::new(w, h));
            if live {
                view.viewDidEndLiveResize();
            }
            Ok(())
        }
        Command::Move { x, y } => {
            window.setFrameOrigin(NSPoint::new(x, y));
            Ok(())
        }
        Command::Cursor(style) => {
            view.state().set_cursor(style);
            Ok(())
        }
        Command::TabPlus => {
            // SAFETY: newWindowForTab: takes a sender, which may be nil.
            let handled = unsafe { window.tryToPerform_with(sel!(newWindowForTab:), None) };
            if handled {
                Ok(())
            } else {
                Err("nothing in the window's responder chain handles newWindowForTab:".into())
            }
        }
        Command::TabDoubleClick(n) => {
            let app = app();
            let at = app
                .tab_center(window, n)
                .filter(|at| app.tab_under(window, *at).is_some())
                .ok_or("the window shows no such tab")?;
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                NSEventType::LeftMouseDown,
                at,
                NSEventModifierFlags::empty(),
                now,
                window.windowNumber(),
                None,
                0,
                2,
                1.0,
            )
            .ok_or("AppKit refused to make the mouse event")?;
            // Through the window, where a click on the tab bar arrives. The
            // window keeps a double click on a tab for itself, so AppKit
            // does not start following a mouse that is not there.
            window.sendEvent(&event);
            Ok(())
        }
        Command::TabPick { window: id, n } => {
            let app = app();
            if app.tab_pointer().is_some() {
                return Err("a tab is already held".into());
            }
            // The tab bar is in the window its tab group shows.
            let tab = app.window(id).ok_or("no such window")?;
            let selected = tab.tabGroup().and_then(|group| group.selectedWindow());
            let host = selected.unwrap_or(tab);
            let at = app
                .tab_center(&host, n)
                .filter(|at| app.tab_under(&host, *at).is_some())
                .ok_or("the window shows no such tab")?;
            let press = tab_mouse(NSEventType::LeftMouseDown, at, &host, now)?;
            app.set_tab_pointer((host.windowNumber(), host.convertPointToScreen(at)));
            // Posted, for the event loop to hand to the window: the press
            // does not come back until the tab is let go, and a request
            // that waited for it would keep every later one waiting too.
            NSApplication::sharedApplication(mtm).postEvent_atStart(&press, false);
            Ok(())
        }
        Command::TabDrag { window: id, x, y } => {
            let app = app();
            let ns = NSApplication::sharedApplication(mtm);
            let (number, from) = app.tab_pointer().ok_or("no tab is held")?;
            let host = ns
                .windowWithWindowNumber(number)
                .ok_or("the window the tab was held in is gone")?;
            let over = app.window(id).ok_or("no such window")?;
            let over_view = app.view_of(&over).ok_or("no such window")?;
            let to =
                over.convertPointToScreen(over_view.convertPoint_toView(NSPoint::new(x, y), None));
            // In steps, as a hand moves it: AppKit takes a tab off its bar
            // when it sees the pointer leave.
            for step in 1..=TAB_DRAG_STEPS {
                let t = f64::from(step) / f64::from(TAB_DRAG_STEPS);
                let on_screen =
                    NSPoint::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
                let at = host.convertPointFromScreen(on_screen);
                let drag = tab_mouse(NSEventType::LeftMouseDragged, at, &host, now)?;
                ns.postEvent_atStart(&drag, false);
            }
            app.set_tab_pointer((number, to));
            Ok(())
        }
        Command::TabDrop => {
            let ns = NSApplication::sharedApplication(mtm);
            let (number, at) = app().tab_pointer().ok_or("no tab is held")?;
            let host = ns
                .windowWithWindowNumber(number)
                .ok_or("the window the tab was held in is gone")?;
            let at = host.convertPointFromScreen(at);
            let release = tab_mouse(NSEventType::LeftMouseUp, at, &host, now)?;
            ns.postEvent_atStart(&release, false);
            Ok(())
        }
        Command::CloseButton => {
            // What a click on the button does, without the click: the
            // window's delegate is asked, then the window closes.
            window.performClose(None);
            Ok(())
        }
        Command::TabDetach => {
            window.moveTabToNewWindow(None);
            Ok(())
        }
        Command::TabMerge => {
            window.mergeAllWindows(None);
            Ok(())
        }
        Command::Marked(text) => {
            let none = NSRange::new(NSNotFound as usize, 0);
            // SAFETY: an NSString is a valid argument for setMarkedText:.
            unsafe {
                view.setMarkedText_selectedRange_replacementRange(
                    &NSString::from_str(&text),
                    NSRange::new(text.encode_utf16().count(), 0),
                    none,
                )
            };
            Ok(())
        }
        Command::BackingScale(scale) => {
            // What viewDidChangeBackingProperties: hands the window state.
            // AppKit's own scale comes back with the next real change.
            let state = view.state();
            state.set_geometry(state.size_pt(), scale);
            Ok(())
        }
        Command::FocusWindow(on) => {
            // What windowDidBecomeKey: and windowDidResignKey: do, without
            // taking keyboard focus from whatever app has it.
            let app = app();
            match app.id_of(window) {
                Some(id) if on => app.window_became_key(id),
                _ => view.state().set_focused(on),
            }
            Ok(())
        }
        Command::State
        | Command::Menu
        | Command::MenuClick(_)
        | Command::Panes
        | Command::Stats
        | Command::StatsReset
        | Command::Action(_)
        | Command::Clipboard
        | Command::SetClipboard(_)
        | Command::Screen(_)
        | Command::Drawn(_)
        | Command::Selection(_)
        | Command::Screenshot(_)
        | Command::WindowImage(_)
        | Command::Confirm(_)
        | Command::FocusPane(_)
        | Command::TabCenter(_)
        | Command::Open(_)
        | Command::Reopen
        | Command::Quit => Ok(()),
    }
}

/// The window's pixels as the compositor has them, so with every layer and
/// the title bar, which an offscreen drawing of the scene leaves out. A
/// process may read its own windows without the screen recording permission
/// that screencapture needs.
fn window_image(window: &NSWindow) -> Option<tern_render::Image> {
    let number = u32::try_from(window.windowNumber()).ok()?;
    // SAFETY: reading a framework constant.
    let anywhere = unsafe { CGRectNull };
    // The replacement Apple names, ScreenCaptureKit, needs that permission
    // and a stream; this is one call.
    #[allow(deprecated)]
    let image = objc2_core_graphics::CGWindowListCreateImage(
        anywhere,
        CGWindowListOption::OptionIncludingWindow,
        number,
        CGWindowImageOption::BoundsIgnoreFraming | CGWindowImageOption::NominalResolution,
    )?;
    let image = Some(&*image);
    let (w, h, row) = (
        CGImage::width(image),
        CGImage::height(image),
        CGImage::bytes_per_row(image),
    );
    if CGImage::bits_per_pixel(image) != 32 || w == 0 || row < w * 4 {
        return None;
    }
    let bytes = CGDataProvider::data(CGImage::data_provider(image).as_deref())?.to_vec();
    if bytes.len() < row * h {
        return None;
    }
    // Window images are BGRA; the title bar's corners are the only pixels
    // that are not opaque.
    let mut rgba = Vec::with_capacity(w * h * 4);
    for line in bytes.chunks_exact(row).take(h) {
        for p in line[..w * 4].chunks_exact(4) {
            rgba.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    Some(tern_render::Image {
        width: u32::try_from(w).ok()?,
        height: u32::try_from(h).ok()?,
        rgba,
    })
}

/// How many steps a tab-drag request moves the held tab in.
const TAB_DRAG_STEPS: u32 = 12;

/// A left mouse event at a point of a window, for a tab the automation
/// socket holds.
fn tab_mouse(
    ty: NSEventType,
    at: NSPoint,
    window: &NSWindow,
    now: f64,
) -> Result<Retained<NSEvent>, String> {
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        ty,
        at,
        NSEventModifierFlags::empty(),
        now,
        window.windowNumber(),
        None,
        0,
        if ty == NSEventType::LeftMouseDragged { 0 } else { 1 },
        if ty == NSEventType::LeftMouseUp { 0.0 } else { 1.0 },
    )
    .ok_or_else(|| "AppKit refused to make the mouse event".to_string())
}

fn send_key(
    mtm: MainThreadMarker,
    window: &NSWindow,
    spec: &KeySpec,
    now: f64,
) -> Result<(), String> {
    let flags = NSEventModifierFlags(spec.flags);
    let make = |ty: NSEventType, characters: &str, ignoring: &str| {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            ty,
            NSPoint::new(0.0, 0.0),
            flags,
            now,
            window.windowNumber(),
            None,
            &NSString::from_str(characters),
            &NSString::from_str(ignoring),
            false,
            spec.code,
        )
        .ok_or_else(|| "AppKit refused to make the key event".to_string())
    };
    let (characters, ignoring) = match spec.fixed {
        Some(c) => (c.to_string(), c.to_string()),
        None => {
            // Ask the current keyboard layout what this key types, as a
            // real key event would carry.
            let probe = make(NSEventType::KeyDown, "", "")?;
            let typing = NSEventModifierFlags(
                spec.flags & (flag::SHIFT | flag::OPTION | flag::CONTROL | flag::CAPS_LOCK),
            );
            let shift_only = NSEventModifierFlags(spec.flags & (flag::SHIFT | flag::CAPS_LOCK));
            let (us_typed, us_ignoring) = spec.us_characters();
            let ask = |m| {
                probe
                    .charactersByApplyingModifiers(m)
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
            };
            (
                ask(typing).unwrap_or(us_typed),
                ask(shift_only).unwrap_or(us_ignoring),
            )
        }
    };
    let down = make(NSEventType::KeyDown, &characters, &ignoring)?;
    let up = make(NSEventType::KeyUp, &characters, &ignoring)?;
    // NSApplication's order for a key press: key equivalents in the key
    // window, then the main menu, then the window's first responder.
    let menu: Option<Retained<NSMenu>> = NSApplication::sharedApplication(mtm).mainMenu();
    let taken =
        window.performKeyEquivalent(&down) || menu.is_some_and(|m| m.performKeyEquivalent(&down));
    if !taken {
        window.sendEvent(&down);
    }
    window.sendEvent(&up);
    Ok(())
}
