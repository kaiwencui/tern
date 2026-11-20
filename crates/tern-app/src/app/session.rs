//! Saving and restoring open windows, tabs, split trees and working
//! directories in the app's state directory.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2_app_kit::{NSScreen, NSWindow, NSWindowTabbingMode};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use tern_config::{Config, WindowSaveState};
use tern_layout::Layout;
use tern_types::PaneId;

use super::{App, Placement};
use crate::identity;
use crate::pane::{cell_size, new_pane_id, FrameRequest, Launch, Pane};
use crate::Options;

#[derive(Clone, Debug, PartialEq)]
pub struct SavedSession {
    pub groups: Vec<SavedGroup>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedGroup {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub font_size: f32,
    pub selected: usize,
    pub tabs: Vec<SavedTab>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedTab {
    pub name: Option<String>,
    pub layout: String,
    pub panes: Vec<(PaneId, Option<PathBuf>)>,
}

impl SavedSession {
    pub fn first_working_dir(&self) -> Option<PathBuf> {
        self.groups
            .first()?
            .tabs
            .first()?
            .panes
            .first()
            .and_then(|(_, dir)| dir.clone())
            .filter(|p| p.is_dir())
    }
}

pub fn should_restore(config: &Config, options: &Options) -> bool {
    config.window_save_state != WindowSaveState::Never
        && options.command.is_empty()
        && options.paths.is_empty()
}

pub fn session_file_path(records_input: bool) -> Option<PathBuf> {
    session_file_path_in(
        std::env::var_os("TERN_SESSION_FILE"),
        records_input,
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
        identity::state_dir_name(),
    )
}

fn session_file_path_in(
    explicit: Option<OsString>,
    records_input: bool,
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
    app_directory: &str,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        let p = PathBuf::from(explicit);
        return (!p.as_os_str().is_empty()).then_some(p);
    }
    if records_input {
        return None;
    }
    // The XDG base directory spec says to ignore a relative path.
    let state_home = xdg_state_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            let home = home.map(PathBuf::from).filter(|dir| dir.is_absolute())?;
            Some(home.join(".local/state"))
        })?;
    Some(state_home.join(app_directory).join("session"))
}

pub fn load_saved(config: &Config, options: &Options) -> Option<SavedSession> {
    if !should_restore(config, options) {
        return None;
    }
    let path = session_file_path(options.control.is_some())?;
    let text = fs::read_to_string(path).ok()?;
    parse_session(&text).filter(|s| !s.groups.is_empty())
}

pub fn serialize_session(session: &SavedSession) -> String {
    let mut out = String::from("tern-session 1\n");
    for g in &session.groups {
        let _ = writeln!(
            out,
            "group {} {} {} {} {} {}",
            g.x, g.y, g.w, g.h, g.font_size, g.selected
        );
        for tab in &g.tabs {
            let name = tab.name.as_deref().map_or_else(|| "-".to_owned(), escape_field);
            let _ = writeln!(out, "tab {name}");
            let _ = writeln!(out, "layout {}", tab.layout);
            for (id, cwd) in &tab.panes {
                let dir = cwd
                    .as_ref()
                    .and_then(|p| p.to_str())
                    .map_or_else(|| "-".to_owned(), escape_field);
                let _ = writeln!(out, "pane {id} {dir}");
            }
        }
    }
    out
}

pub fn parse_session(text: &str) -> Option<SavedSession> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next()? != "tern-session 1" {
        return None;
    }
    let mut groups: Vec<SavedGroup> = Vec::new();
    for line in lines {
        let (kind, rest) = line.split_once(' ')?;
        match kind {
            "group" => {
                let mut parts = rest.split_whitespace();
                let x: f64 = parts.next()?.parse().ok()?;
                let y: f64 = parts.next()?.parse().ok()?;
                let w: f64 = parts.next()?.parse().ok()?;
                let h: f64 = parts.next()?.parse().ok()?;
                let font_size: f32 = parts.next()?.parse().ok()?;
                let selected: usize = parts.next()?.parse().ok()?;
                if !x.is_finite()
                    || !y.is_finite()
                    || !w.is_finite()
                    || !h.is_finite()
                    || w < 80.0
                    || h < 50.0
                {
                    continue;
                }
                groups.push(SavedGroup {
                    x,
                    y,
                    w,
                    h,
                    font_size: font_size.clamp(6.0, 72.0),
                    selected,
                    tabs: Vec::new(),
                });
            }
            "tab" => {
                let group = groups.last_mut()?;
                let name = (rest != "-").then(|| unescape_field(rest));
                group.tabs.push(SavedTab {
                    name,
                    layout: String::new(),
                    panes: Vec::new(),
                });
            }
            "layout" => {
                let tab = groups.last_mut()?.tabs.last_mut()?;
                tab.layout = rest.to_owned();
            }
            "pane" => {
                let tab = groups.last_mut()?.tabs.last_mut()?;
                let (id_str, dir_str) = rest.split_once(' ')?;
                let id: PaneId = id_str.parse().ok()?;
                let dir = (dir_str != "-").then(|| PathBuf::from(unescape_field(dir_str)));
                tab.panes.push((id, dir));
            }
            _ => {}
        }
    }
    groups.retain(|g| !g.tabs.is_empty() && g.tabs.iter().all(|t| !t.panes.is_empty()));
    Some(SavedSession { groups })
}

fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn unescape_field(s: &str) -> String {
    let inner = s
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(s);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl App {
    /// Captures all currently open tab groups and split trees.
    pub(super) fn snapshot_session(&self) -> SavedSession {
        let mut seen_ids = HashSet::new();
        let mut groups = Vec::new();

        for (_, window, _) in self.all_windows() {
            let Some(first_id) = self.id_of(&window) else {
                continue;
            };
            if seen_ids.contains(&first_id) {
                continue;
            }
            let members: Vec<Retained<NSWindow>> = match window.tabbedWindows() {
                Some(arr) if !arr.is_empty() => arr.to_vec(),
                _ => vec![window.clone()],
            };
            let selected_win = window
                .tabGroup()
                .and_then(|g| g.selectedWindow())
                .unwrap_or_else(|| window.clone());
            let selected_id = self.id_of(&selected_win);

            let frame = selected_win.frame();
            let mut selected_idx = 0usize;
            let mut font_size = self.config().font_size;
            let mut tabs = Vec::new();

            for member in &members {
                let Some(id) = self.id_of(member) else {
                    continue;
                };
                seen_ids.insert(id);
                let Some(state) = self.state(id) else {
                    continue;
                };
                if Some(id) == selected_id {
                    selected_idx = tabs.len();
                    font_size = state.font_size();
                }
                let layout = state.layout_text();
                let parsed = match layout.parse::<Layout>() {
                    Ok(l) if !l.is_empty() => l,
                    _ => continue,
                };
                let order = parsed.panes();
                let panes = state.with_panes(|ps| {
                    order
                        .iter()
                        .map(|&pid| {
                            let cwd = ps
                                .iter()
                                .find(|p| p.id == pid)
                                .and_then(|p| p.session.cwd());
                            (pid, cwd)
                        })
                        .collect::<Vec<_>>()
                });
                tabs.push(SavedTab {
                    name: state.name(),
                    layout,
                    panes,
                });
            }

            if !tabs.is_empty() {
                groups.push(SavedGroup {
                    x: frame.origin.x,
                    y: frame.origin.y,
                    w: frame.size.width,
                    h: frame.size.height,
                    font_size,
                    selected: selected_idx,
                    tabs,
                });
            }
        }

        SavedSession { groups }
    }

    pub(super) fn persist_session_on_quit(&self) {
        if !self.options.command.is_empty() {
            return;
        }
        let Some(path) = session_file_path(self.records_input()) else {
            return;
        };
        let mode = self.config().window_save_state;
        if mode == WindowSaveState::Never {
            let _ = fs::remove_file(&path);
            return;
        }
        let mut current = self.snapshot_session();
        if current.groups.is_empty() && mode == WindowSaveState::Always
            && let Some(last) = self.last_session.borrow().clone() {
                current = last;
            }
        if current.groups.is_empty() {
            let _ = fs::remove_file(&path);
            return;
        }
        write_session_atomic(&path, &serialize_session(&current));
    }

    pub(super) fn remember_closing_last_window(&self) {
        if self.config().window_save_state == WindowSaveState::Always
            && self.options.command.is_empty()
            && self.windows.borrow().len() == 1
        {
            let snap = self.snapshot_session();
            if !snap.groups.is_empty() {
                *self.last_session.borrow_mut() = Some(snap);
            }
        }
    }

    /// Restores windows, tabs, split layouts and working directories, reusing
    /// `first_pane` for the very first leaf pane so launch stays instant.
    pub(super) fn restore_saved_session(&self, saved: SavedSession, first_pane: Pane) {
        let mut first_pane = Some(first_pane);
        let config = self.config();

        for group in saved.groups {
            let mut group_windows: Vec<Retained<NSWindow>> = Vec::new();
            for tab in group.tabs {
                let parsed = match tab.layout.parse::<Layout>() {
                    Ok(l) if l.panes().len() == tab.panes.len() => l,
                    _ => continue,
                };
                let first_dir = tab
                    .panes
                    .first()
                    .and_then(|(_, d)| d.clone())
                    .filter(|d| d.is_dir());
                let initial_pane = first_pane.take();
                let placement = match group_windows.first() {
                    Some(host) => Placement::TabOf(host.clone()),
                    None => Placement::Window,
                };
                self.create_window(placement, initial_pane, first_dir);
                let Some(created) = self.windows.borrow().last().map(|t| {
                    (Rc::clone(&t.state), t.window.clone())
                }) else {
                    continue;
                };
                let (state, win) = created;
                state.set_font_size(group.font_size);
                if let Some(name) = tab.name {
                    state.set_name(Some(name));
                }

                if group_windows.is_empty() {
                    let clamped = clamp_frame_to_screens(
                        self.mtm,
                        NSRect::new(NSPoint::new(group.x, group.y), NSSize::new(group.w, group.h)),
                    );
                    win.setTabbingMode(NSWindowTabbingMode::Disallowed);
                    win.setFrame_display(clamped, true);
                    win.setTabbingMode(NSWindowTabbingMode::Preferred);
                }

                if tab.panes.len() > 1 {
                    let metrics = state.metrics();
                    let mut extra = Vec::with_capacity(tab.panes.len() - 1);
                    let mut ok = true;
                    for (_, dir) in &tab.panes[1..] {
                        let launch = Launch {
                            id: new_pane_id(),
                            command: None,
                            working_dir: dir.clone().filter(|d| d.is_dir()),
                            cols: 20,
                            rows: 8,
                            cell: cell_size(&metrics),
                        };
                        match Pane::spawn(&config, launch, Arc::new(FrameRequest::default())) {
                            Ok(p) => extra.push(p),
                            Err(e) => {
                                eprintln!("tern: cannot restore split pane: {e}");
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        state.restore_splits(parsed, extra);
                    }
                }

                group_windows.push(win);
            }

            if let Some(sel) = group_windows.get(group.selected) {
                if let Some(tg) = sel.tabGroup() {
                    tg.setSelectedWindow(Some(sel));
                }
                self.show(sel);
            }
        }
    }
}

fn clamp_frame_to_screens(mtm: objc2::MainThreadMarker, rect: NSRect) -> NSRect {
    let screens = NSScreen::screens(mtm);
    let Some(target) = screens
        .iter()
        .find(|s| {
            let f = s.visibleFrame();
            let cx = rect.origin.x + rect.size.width * 0.5;
            let cy = rect.origin.y + rect.size.height * 0.5;
            cx >= f.origin.x
                && cx <= f.origin.x + f.size.width
                && cy >= f.origin.y
                && cy <= f.origin.y + f.size.height
        })
        .or_else(|| NSScreen::mainScreen(mtm))
    else {
        return rect;
    };
    let v = target.visibleFrame();
    let w = rect.size.width.min(v.size.width).max(120.0);
    let h = rect.size.height.min(v.size.height).max(80.0);
    let x = rect.origin.x.clamp(v.origin.x, v.origin.x + v.size.width - w);
    let y = rect
        .origin
        .y
        .clamp(v.origin.y, v.origin.y + v.size.height - h);
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn write_session_atomic(path: &Path, data: &str) {
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err() {
            return;
        }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("session");
    let tmp = path.with_file_name(format!(".{name}.tmp.{}", std::process::id()));
    if fs::write(&tmp, data)
        .and_then(|()| fs::rename(&tmp, path))
        .is_err()
    {
        let _ = fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_file_path_respects_variant_and_xdg_spec() {
        let os = |s: &str| Some(OsString::from(s));

        assert_eq!(
            session_file_path_in(None, false, None, os("/Users/kai"), "tern"),
            Some(PathBuf::from("/Users/kai/.local/state/tern/session"))
        );
        assert_eq!(
            session_file_path_in(None, false, None, os("/Users/kai"), "tern-next"),
            Some(PathBuf::from("/Users/kai/.local/state/tern-next/session"))
        );
        assert_eq!(
            session_file_path_in(None, false, os("/tmp/state"), os("/Users/kai"), "tern-next"),
            Some(PathBuf::from("/tmp/state/tern-next/session"))
        );
        // Relative XDG_STATE_HOME is ignored per XDG spec.
        assert_eq!(
            session_file_path_in(None, false, os("relative/state"), os("/Users/kai"), "tern-next"),
            Some(PathBuf::from("/Users/kai/.local/state/tern-next/session"))
        );
        assert_eq!(
            session_file_path_in(None, false, os("relative"), os("also-relative"), "tern"),
            None
        );
        // Automation mode skips default session file unless explicitly overridden.
        assert_eq!(
            session_file_path_in(None, true, os("/tmp/state"), os("/Users/kai"), "tern"),
            None
        );
        assert_eq!(
            session_file_path_in(os("/tmp/custom"), true, None, None, "tern"),
            Some(PathBuf::from("/tmp/custom"))
        );
        assert_eq!(
            session_file_path_in(os(""), false, os("/tmp/state"), os("/Users/kai"), "tern"),
            None
        );
    }

    #[test]
    fn session_text_round_trips_groups_tabs_splits_and_paths() {
        let sample = SavedSession {
            groups: vec![SavedGroup {
                x: 120.0,
                y: 80.0,
                w: 960.0,
                h: 640.0,
                font_size: 15.0,
                selected: 1,
                tabs: vec![
                    SavedTab {
                        name: Some("work \"main\"".into()),
                        layout: "h0.5(1,v0.25(!2,3))".into(),
                        panes: vec![
                            (1, Some(PathBuf::from("/Users/kai/work/tern"))),
                            (2, Some(PathBuf::from("/tmp/with spaces"))),
                            (3, None),
                        ],
                    },
                    SavedTab {
                        name: None,
                        layout: "*4".into(),
                        panes: vec![(4, Some(PathBuf::from("/var/tmp")))],
                    },
                ],
            }],
        };
        let encoded = serialize_session(&sample);
        let decoded = parse_session(&encoded).expect("valid session");
        assert_eq!(decoded, sample);
    }
}
