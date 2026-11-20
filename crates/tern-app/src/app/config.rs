//! The configuration at run time: reloading it, opening its file, and
//! telling the user what is wrong with it.

use std::path::Path;
use std::rc::Rc;

use objc2_app_kit::{NSApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{ns_string, NSArray, NSString, NSURL};
use tern_config::Config;
use tern_render::{FontConfig, RenderError};

use super::close::{Question, Subject};
use super::App;
use crate::menu;

/// The most problems one alert lists; the rest are counted.
const LISTED_PROBLEMS: usize = 12;

impl App {
    /// Reads the configuration again and applies it to every open window:
    /// font, colors, padding, cursor, dimming, dividers and key bindings
    /// (menus included). Settings read as they are needed (Option as Alt,
    /// copy on select and the like) follow by themselves. Those that only
    /// shape new panes and windows (shell, working directory, scrollback,
    /// window size and position) apply to those.
    pub(super) fn reload_config(&self) {
        let (mut new, diagnostics) =
            Config::load(self.options.config_file.as_deref(), &self.options.overrides);
        let mut problems: Vec<String> = diagnostics.iter().map(ToString::to_string).collect();
        let old = self.config();
        let font = new.font_family != old.font_family || new.font_size != old.font_size;
        if font
            && let Err(e) = self.set_font(&new.font_family, new.font_size) {
                problems.push(format!(
                    "font-family: {e}; keeping {}",
                    font_name(&old.font_family)
                ));
                new.font_family.clone_from(&old.font_family);
                if let Err(e) = self.set_font(&old.font_family, new.font_size) {
                    problems.push(e.to_string());
                }
            }
        let new = Rc::new(new);
        *self.config.borrow_mut() = Rc::clone(&new);
        if old.bindings != new.bindings {
            let ns = NSApplication::sharedApplication(self.mtm);
            menu::update_shortcuts(&ns, &new.bindings, &self.menu_commands.borrow());
        }
        for (state, _, _) in self.all_windows() {
            state.config_changed(&old, &new, font);
        }
        self.report_problems(&problems);
    }

    /// Switches the renderer to another font; the size is remembered once
    /// it works.
    fn set_font(&self, family: &str, size: f32) -> Result<(), RenderError> {
        let font = FontConfig {
            family: family.to_string(),
            size_pt: size,
        };
        self.renderer().set_font(&font)?;
        self.font_size.set(size);
        Ok(())
    }

    /// Tells the user what is wrong with the configuration: on stderr, and
    /// in an alert on the current window.
    pub(super) fn report_problems(&self, problems: &[String]) {
        if problems.is_empty() {
            return;
        }
        for p in problems {
            eprintln!("tern: {p}");
        }
        let mut detail: Vec<String> = problems.iter().take(LISTED_PROBLEMS).cloned().collect();
        if problems.len() > LISTED_PROBLEMS {
            detail.push(format!("and {} more", problems.len() - LISTED_PROBLEMS));
        }
        let number = self.notices.get() + 1;
        self.notices.set(number);
        self.ask(Question {
            window: None,
            subject: Subject::Notice(number),
            message: "The configuration has problems".into(),
            detail: format!(
                "{}\n\nEverything else in it was applied.",
                detail.join("\n")
            ),
            action: "OK",
            cancel: false,
            shown_on: None,
        });
    }

    /// Opens the config file in the user's editor, writing the commented
    /// default configuration first when there is none. Under automation the
    /// file is written but nothing is opened; the opening is recorded.
    pub(super) fn open_config(&self) {
        let Some(path) = self
            .options
            .config_file
            .clone()
            .or_else(tern_config::default_path)
        else {
            self.report_problems(&["no config file path: HOME is not set".into()]);
            return;
        };
        if let Err(e) = ensure_config_file(&path) {
            self.report_problems(&[format!("cannot create {}: {e}", path.display())]);
            return;
        }
        if self.records_input() {
            self.opened_links
                .borrow_mut()
                .push(format!("open {}", path.display()));
            return;
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let workspace = NSWorkspace::sharedWorkspace();
        if workspace.URLForApplicationToOpenURL(&url).is_some() {
            workspace.openURL(&url);
        } else if let Some(editor) =
            workspace.URLForApplicationWithBundleIdentifier(ns_string!("com.apple.TextEdit"))
        {
            // A file with no extension has no default app.
            workspace.openURLs_withApplicationAtURL_configuration_completionHandler(
                &NSArray::from_slice(&[&*url]),
                &editor,
                &NSWorkspaceOpenConfiguration::configuration(),
                None,
            );
        }
    }
}

/// A font family as messages name it; the empty name is the system's font.
pub(super) fn font_name(family: &str) -> String {
    if family.is_empty() {
        "the system's monospaced font".into()
    } else {
        format!("\"{family}\"")
    }
}

/// Writes the commented default configuration where none exists yet.
fn ensure_config_file(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, tern_config::DEFAULT_CONFIG_TEMPLATE)
}
