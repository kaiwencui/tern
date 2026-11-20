//! The automation socket's replies, as JSON written by hand.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAppearanceCustomization, NSApplication, NSColorSpace, NSScreen, NSWindow};
use tern_layout::Axis;
use tern_types::{CursorShape, Mods, MouseTracking, Point, Rect};

use crate::app::app;
use crate::input::{DropPayload, InputRecord, KeyOutcome, MouseKind};
use crate::keymap;
use crate::state::FrameTime;

pub(super) fn error(message: &str) -> String {
    format!("{{\"ok\":false,\"error\":{}}}", string(message))
}

pub(super) fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Bytes sent to a terminal, as a string; control characters come out as
/// JSON's escapes.
fn bytes(b: &[u8]) -> String {
    string(&String::from_utf8_lossy(b))
}

fn float(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".into()
    }
}

fn list<I: IntoIterator<Item = String>>(items: I) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(","))
}

fn opt_string(s: Option<&str>) -> String {
    s.map_or_else(|| "null".into(), string)
}

fn mods(m: Mods) -> String {
    let names = [
        (Mods::SHIFT, "shift"),
        (Mods::ALT, "alt"),
        (Mods::CTRL, "ctrl"),
        (Mods::SUPER, "super"),
        (Mods::CAPS_LOCK, "caps_lock"),
        (Mods::NUM_LOCK, "num_lock"),
    ];
    list(
        names
            .iter()
            .filter(|(bit, _)| m.contains(*bit))
            .map(|(_, n)| string(n)),
    )
}

fn point(p: Point) -> String {
    format!("\"x\":{},\"y\":{}", p.x, p.y)
}

fn rect(r: Rect) -> String {
    format!("[{},{},{},{}]", r.x, r.y, r.w, r.h)
}

pub(super) fn record(seq: u64, record: &InputRecord) -> String {
    let body = match record {
        InputRecord::Key(k, outcome) => {
            let (key, m, left, right) = match k.press {
                Some(p) => (
                    string(&format!("{:?}", p.key)),
                    mods(p.mods),
                    p.option.left,
                    p.option.right,
                ),
                None => (
                    "null".into(),
                    mods(keymap::mods_from_flags(k.flags)),
                    false,
                    false,
                ),
            };
            let outcome = match outcome {
                KeyOutcome::Sent(sent) => format!("\"sent\":{}", bytes(sent)),
                KeyOutcome::Bound(action) => format!("\"action\":{}", string(&action.to_string())),
            };
            format!(
                "\"type\":\"key\",\"down\":{},\"code\":{},\"flags\":{},\"key\":{key},\"mods\":{m},\
                 \"left_option\":{left},\"right_option\":{right},\"characters\":{},\"unmodified\":{},\
                 \"repeat\":{},\"text\":{},\"commands\":{},\"composing\":{},{outcome}",
                k.down,
                k.code,
                k.flags,
                string(&k.characters),
                string(&k.characters_ignoring_modifiers),
                k.repeat,
                string(&k.text),
                list(k.commands.iter().map(|c| string(c))),
                k.composing
            )
        }
        InputRecord::Modifier {
            key,
            down,
            mods: m,
            sent,
        } => format!(
            "\"type\":\"modifier\",\"key\":{},\"down\":{down},\"mods\":{},\"sent\":{}",
            string(&format!("{key:?}")),
            mods(*m),
            bytes(sent)
        ),
        InputRecord::Text { text, sent } => format!(
            "\"type\":\"text\",\"text\":{},\"sent\":{}",
            string(text),
            bytes(sent)
        ),
        InputRecord::Preedit(t) => format!("\"type\":\"preedit\",\"text\":{}", string(t)),
        InputRecord::Mouse(m) => {
            let kind = match m.kind {
                MouseKind::Down => "down",
                MouseKind::Up => "up",
                MouseKind::Drag => "drag",
                MouseKind::Move => "move",
            };
            format!(
                "\"type\":\"mouse\",\"kind\":\"{kind}\",\"button\":{},{},\"mods\":{},\"clicks\":{}",
                m.button,
                point(m.at),
                mods(m.mods),
                m.clicks
            )
        }
        InputRecord::Scroll(s) => format!(
            "\"type\":\"scroll\",{},\"dx\":{},\"dy\":{},\"precise\":{},\"phase\":{},\"momentum\":{},\"mods\":{}",
            point(s.at),
            float(s.dx),
            float(s.dy),
            s.precise,
            s.phase,
            s.momentum,
            mods(s.mods)
        ),
        InputRecord::DragOver(Some(p)) => format!("\"type\":\"drag\",{}", point(*p)),
        InputRecord::DragOver(None) => "\"type\":\"drag_exit\"".into(),
        InputRecord::Drop { at, payload } => match payload {
            DropPayload::Paths(paths) => format!(
                "\"type\":\"drop\",{},\"paths\":{}",
                point(*at),
                list(paths.iter().map(|p| string(&p.to_string_lossy())))
            ),
            DropPayload::Text(t) => {
                format!("\"type\":\"drop\",{},\"text\":{}", point(*at), string(t))
            }
        },
    };
    format!("{{\"seq\":{seq},{body}}}")
}

/// What a window's title bar shows: the window's own background color
/// (as sRGB 0xrrggbb; null when it has no such form), whether the title bar
/// lets it through, and whether the window's appearance is the dark one.
fn titlebar(window: &NSWindow) -> String {
    let color = window
        .backgroundColor()
        .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
        .map_or_else(
            || "null".into(),
            |c| {
                let channel = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u32;
                let rgb = (channel(c.redComponent()) << 16)
                    | (channel(c.greenComponent()) << 8)
                    | channel(c.blueComponent());
                rgb.to_string()
            },
        );
    let dark = window
        .appearance()
        .is_some_and(|a| a.name().to_string().contains("Dark"));
    format!(
        "{{\"color\":{color},\"transparent\":{},\"dark\":{dark}}}",
        window.titlebarAppearsTransparent()
    )
}

pub(super) fn state(mtm: MainThreadMarker) -> String {
    let app = app();
    let ns = NSApplication::sharedApplication(mtm);
    let current = app.current_window().and_then(|w| app.id_of(&w));
    let windows = app.all_windows().into_iter().map(|(state, window, view)| {
        let group = window.tabGroup();
        let tabs = group.as_ref().map_or_else(
            || "[]".into(),
            |g| {
                list(
                    g.windows()
                        .iter()
                        .filter_map(|w| app.id_of(&w))
                        .map(|id| id.to_string()),
                )
            },
        );
        let selected = group
            .as_ref()
            .and_then(|g| g.selectedWindow())
            .and_then(|w| app.id_of(&w));
        let frame = window.frame();
        let ime = view.ime_screen_rect();
        let (w_pt, h_pt) = state.size_pt();
        let (w_px, h_px) = state.size_px();
        let m = state.metrics();
        format!(
            "{{\"id\":{},\"number\":{},\"title\":{},\"tab_name\":{},\"renaming\":{},\"tabs\":{tabs},\
             \"selected_tab\":{},\
             \"frame\":[{},{},{},{}],\"size_pt\":[{},{}],\"size_px\":[{w_px},{h_px}],\"scale\":{},\
             \"cell\":[{},{}],\"key\":{},\"main\":{},\"visible\":{},\"occluded\":{},\"focused\":{},\
             \"frames\":{},\"cursor\":\"{}\",\"ime_rect\":[{},{},{},{}],\"panes\":{},\
             \"focused_pane\":{},\"layout\":{},\"zoomed\":{},\"dividers\":{},\"sheet\":{},\"titlebar\":{},\
             \"pointer_hidden\":{},\"gesture\":\"{}\",\"quads\":{},\"last_seq\":{},\"events\":{}}}",
            state.id,
            window.windowNumber(),
            string(&window.title().to_string()),
            opt_string(state.name().as_deref()),
            app.renaming() == Some(state.id),
            selected.map_or_else(|| "null".into(), |id| id.to_string()),
            float(frame.origin.x),
            float(frame.origin.y),
            float(frame.size.width),
            float(frame.size.height),
            float(w_pt),
            float(h_pt),
            float(state.scale()),
            m.cell_w,
            m.cell_h,
            window.isKeyWindow(),
            window.isMainWindow(),
            window.isVisible(),
            state.occluded(),
            state.focused(),
            state.frames_presented(),
            state.cursor().name(),
            float(ime.origin.x),
            float(ime.origin.y),
            float(ime.size.width),
            float(ime.size.height),
            state.with_panes(|panes| list(panes.iter().map(|p| p.id.to_string()))),
            state
                .focused_pane_id()
                .map_or_else(|| "null".into(), |id| id.to_string()),
            string(&state.layout_text()),
            state.is_zoomed(),
            list(state.dividers().iter().map(|d| format!(
                "{{\"rect\":{},\"axis\":\"{}\"}}",
                rect(d.rect),
                match d.axis {
                    Axis::Horizontal => "horizontal",
                    Axis::Vertical => "vertical",
                }
            ))),
            window.attachedSheet().is_some(),
            titlebar(&window),
            state.pointer_hidden(),
            state.gesture_name(),
            list(state.quads().iter().map(|q| format!(
                "{{\"rect\":{},\"color\":{},\"alpha\":{}}}",
                rect(q.rect),
                q.color,
                float(f64::from(q.alpha))
            ))),
            state.last_seq(),
            list(
                state
                    .records_after(0)
                    .iter()
                    .map(|(seq, r)| record(*seq, r))
            )
        )
    });
    let windows = list(windows);
    let screens = list(NSScreen::screens(mtm).iter().map(|screen| {
        let (f, v) = (screen.frame(), screen.visibleFrame());
        format!(
            "{{\"frame\":[{},{},{},{}],\"visible\":[{},{},{},{}],\"scale\":{}}}",
            float(f.origin.x),
            float(f.origin.y),
            float(f.size.width),
            float(f.size.height),
            float(v.origin.x),
            float(v.origin.y),
            float(v.size.width),
            float(v.size.height),
            float(screen.backingScaleFactor())
        )
    }));
    let opts = &app.options;
    let sessions: usize = app
        .all_windows()
        .iter()
        .map(|(state, _, _)| state.with_panes(<[_]>::len))
        .sum();
    format!(
        "{{\"ok\":true,\"pid\":{},\"active\":{},\"current\":{},\"command\":{},\"config_file\":{},\
         \"font_size\":{},\"sessions\":{sessions},\"live_sessions\":{},\"opened_links\":{},\
         \"questions\":{},\"tab_drag\":{},\"screens\":{screens},\"windows\":{windows}}}",
        std::process::id(),
        ns.isActive(),
        current.map_or_else(|| "null".into(), |id| id.to_string()),
        list(opts.command.iter().map(|a| string(a))),
        opt_string(
            opts.config_file
                .as_ref()
                .map(|p| p.to_string_lossy())
                .as_deref()
        ),
        float(f64::from(app.font_size())),
        crate::pane::live_panes(),
        list(app.opened_links().iter().map(|l| string(l))),
        list(
            app.pending_questions()
                .iter()
                .map(|(message, shown)| format!(
                    "{{\"message\":{},\"shown\":{shown}}}",
                    string(message)
                ))
        ),
        app.tab_dragged()
            .map_or_else(|| "null".into(), |id| id.to_string()),
    )
}

/// Every window's panes, with what tests need to check a pane against the
/// program running in it.
pub(super) fn panes() -> String {
    let windows = app().all_windows().into_iter().map(|(state, _, _)| {
        state.refresh();
        let focused = state.focused_pane_id();
        let panes = state.with_panes(|panes| {
            list(panes.iter().map(|p| {
                let (cols, rows) = p.grid;
                let m = p.session.modes();
                let mouse = match m.mouse {
                    MouseTracking::Off => "off",
                    MouseTracking::Click => "click",
                    MouseTracking::Drag => "drag",
                    MouseTracking::Motion => "motion",
                };
                let cursor = p.frame.cursor.map_or_else(
                    || "null".into(),
                    |c| {
                        let shape = match c.shape {
                            CursorShape::Block => "block",
                            CursorShape::Beam => "beam",
                            CursorShape::Underline => "underline",
                            CursorShape::HollowBlock => "hollow",
                        };
                        format!(
                            "{{\"row\":{},\"col\":{},\"shape\":\"{shape}\",\"blinking\":{}}}",
                            c.row, c.col, c.blinking
                        )
                    },
                );
                format!(
                    "{{\"id\":{},\"focused\":{},\"shown\":{},\"selection\":{},\"rect\":{},\"origin\":[{},{}],\"grid\":[{cols},{rows}],\
                     \"frame_grid\":[{},{}],\"title\":{},\"cwd\":{},\"pid\":{},\"alive\":{},\
                     \"foreground\":{},\"cursor\":{cursor},\"display_offset\":{},\"history\":{},\
                     \"modes\":{{\"app_cursor\":{},\"app_keypad\":{},\"bracketed_paste\":{},\
                     \"focus_events\":{},\"alt_screen\":{},\"mouse\":\"{mouse}\",\"kitty_flags\":{}}}}}",
                    p.id,
                    Some(p.id) == focused,
                    p.shown,
                    p.session.has_selection(),
                    rect(p.rect),
                    p.origin.x,
                    p.origin.y,
                    p.frame.cols,
                    p.frame.rows,
                    opt_string(p.title.as_deref()),
                    opt_string(p.session.cwd().as_ref().map(|d| d.to_string_lossy()).as_deref()),
                    p.session.child_pid(),
                    p.session.is_alive(),
                    opt_string(p.session.foreground_process().as_deref()),
                    p.frame.display_offset,
                    p.frame.history,
                    m.app_cursor,
                    m.app_keypad,
                    m.bracketed_paste,
                    m.focus_events,
                    m.alt_screen,
                    m.kitty_flags,
                )
            }))
        });
        format!("{{\"window\":{},\"panes\":{panes}}}", state.id)
    });
    format!("{{\"ok\":true,\"windows\":{}}}", list(windows))
}

/// Frame counts, the main thread's recent frame times, and the process's
/// CPU time so far.
pub(super) fn stats() -> String {
    let app = app();
    let windows = app.all_windows().into_iter().map(|(state, _, _)| {
        let times = state.frame_times();
        let summary = |pick: &dyn Fn(&FrameTime) -> u64| {
            let mut v: Vec<u64> = times.recent().map(pick).collect();
            v.sort_unstable();
            let median = v.get(v.len() / 2).copied().unwrap_or(0);
            let max = v.last().copied().unwrap_or(0);
            format!("{{\"median\":{median},\"max\":{max}}}")
        };
        let micros = |pick: fn(&FrameTime) -> std::time::Duration| {
            summary(&|t| pick(t).as_micros() as u64)
        };
        let keys = state.key_latency();
        format!(
            "{{\"id\":{},\"frames\":{},\"draws\":{},\"link_running\":{},\"blinking\":{},\
             \"blinks\":{},\"covered\":{},\
             \"timed_frames\":{},\"snapshots\":{},\"snapshot_us\":{},\"prepare_us\":{},\"encode_us\":{},\
             \"total_us\":{},\"key_latency_us\":{{\"count\":{},\"median\":{},\"max\":{}}}}}",
            state.id,
            state.frames_presented(),
            state.draws(),
            state.link_running(),
            state.blinking(),
            state.blinks(),
            state.covered(),
            times.recent().count(),
            summary(&|t| u64::from(t.snapshots)),
            micros(|t| t.snapshot),
            micros(|t| t.prepare),
            micros(|t| t.encode),
            micros(|t| t.total),
            keys.count,
            keys.median.as_micros(),
            keys.max.as_micros(),
        )
    });
    format!(
        "{{\"ok\":true,\"cpu_us\":{},\"bells\":{},\"windows\":{}}}",
        process_cpu_micros(),
        app.bells(),
        list(windows)
    )
}

/// User plus system CPU time of the whole process.
fn process_cpu_micros() -> u64 {
    // SAFETY: rusage is plain data, so all zeroes is a valid value.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: the pointer is to a live rusage.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return 0;
    }
    let micros = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    micros(usage.ru_utime) + micros(usage.ru_stime)
}

pub(super) fn menu(mtm: MainThreadMarker) -> String {
    let Some(main) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return error("no main menu");
    };
    let tops = main.itemArray();
    let menus = tops.iter().filter_map(|top| top.submenu()).map(|menu| {
        // Runs validation, as opening the menu would.
        menu.update();
        let items = menu.itemArray();
        let items = items
            .iter()
            .map(|item| {
                if item.isSeparatorItem() {
                    return "{\"separator\":true}".to_string();
                }
                let command = app().menu_command(item.tag());
                format!(
                    "{{\"title\":{},\"command\":{},\"key\":{},\"mods\":{},\"hidden\":{},\"enabled\":{},\"action\":{}}}",
                    string(&item.title().to_string()),
                    opt_string(command.map(|c| c.name()).as_deref()),
                    string(&item.keyEquivalent().to_string()),
                    mods(keymap::mods_from_flags(item.keyEquivalentModifierMask().0)),
                    item.isHidden(),
                    item.isEnabled(),
                    item.action()
                        .map_or_else(|| "null".into(), |a| string(&a.name().to_string_lossy())),
                )
            })
            .collect::<Vec<_>>();
        format!(
            "{{\"title\":{},\"items\":{}}}",
            string(&menu.title().to_string()),
            list(items)
        )
    });
    format!("{{\"ok\":true,\"menus\":{}}}", list(menus))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_are_escaped() {
        let expected = format!("\"a\\\"b\\\\c\\n{}u0001é\"", '\\');
        assert_eq!(string("a\"b\\c\n\u{1}é"), expected);
        assert_eq!(float(1.5), "1.5");
        assert_eq!(float(f64::NAN), "null");
        assert_eq!(mods(Mods::SHIFT | Mods::CTRL), "[\"shift\",\"ctrl\"]");
        assert_eq!(error("x"), "{\"ok\":false,\"error\":\"x\"}");
        assert_eq!(bytes(b"\x1bb"), format!("\"{}u001bb\"", '\\'));
    }

    #[test]
    fn records_render() {
        let r = InputRecord::Drop {
            at: Point { x: 3, y: 4 },
            payload: DropPayload::Paths(vec!["/tmp/a b".into()]),
        };
        assert_eq!(
            record(7, &r),
            "{\"seq\":7,\"type\":\"drop\",\"x\":3,\"y\":4,\"paths\":[\"/tmp/a b\"]}"
        );
        let r = InputRecord::Text {
            text: "é".into(),
            sent: "é".into(),
        };
        assert_eq!(
            record(1, &r),
            "{\"seq\":1,\"type\":\"text\",\"text\":\"é\",\"sent\":\"é\"}"
        );
    }
}
