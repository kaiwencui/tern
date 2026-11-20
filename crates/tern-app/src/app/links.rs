//! Opening the links and local file paths the user Command-clicks.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSArray, NSString, NSURL};

use super::App;
use crate::links::{ClickTarget, FileTarget};

impl App {
    pub fn open_target(&self, target: &ClickTarget) {
        match target {
            ClickTarget::Url(url) => self.open_link(url),
            ClickTarget::File(file) => self.open_file(file),
        }
    }

    /// Opens a link the user Command-clicked: a web or mail link in its
    /// default app, a file link by showing the file in the Finder. Links of
    /// any other kind are refused: a program's output could otherwise make
    /// an innocent-looking word open an app or a custom URL scheme. Under
    /// automation the link is recorded instead, so tests open nothing.
    pub fn open_link(&self, url: &str) {
        let kind = link_kind(url);
        if kind == LinkKind::Refused {
            eprintln!("tern: not opening {url}: only web, mail and file links open");
            return;
        }
        if self.records_input() {
            let record = match kind {
                LinkKind::Reveal => format!("reveal {url}"),
                _ => url.to_string(),
            };
            self.opened_links.borrow_mut().push(record);
            return;
        }
        let Some(link) = NSURL::URLWithString(&NSString::from_str(url)) else {
            eprintln!("tern: not a link: {url}");
            return;
        };
        let workspace = NSWorkspace::sharedWorkspace();
        match kind {
            LinkKind::Reveal => {
                workspace.activateFileViewerSelectingURLs(&NSArray::from_slice(&[&*link]))
            }
            _ => {
                if !workspace.openURL(&link) {
                    eprintln!("tern: nothing opens {url}");
                }
            }
        }
    }

    /// Opens a resolved local file or directory Command-clicked in a pane.
    pub fn open_file(&self, target: &FileTarget) {
        if self.records_input() {
            let mut rec = format!("file {}", target.path.display());
            if let Some(line) = target.line {
                rec.push_str(&format!(":{line}"));
                if let Some(col) = target.col {
                    rec.push_str(&format!(":{col}"));
                }
            }
            self.opened_links.borrow_mut().push(rec);
            return;
        }

        if let Some(template) = &self.config().open_command {
            let argv = expand_open_command(template, target);
            if let Some((prog, args)) = argv.split_first() {
                if let Err(e) = Command::new(prog).args(args).spawn() {
                    eprintln!("tern: open-command {prog}: {e}");
                }
                return;
            }
        }

        let path_str = target.path.to_string_lossy();
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path_str));
        let workspace = NSWorkspace::sharedWorkspace();
        if should_reveal_only(&target.path) || !workspace.openURL(&url) {
            workspace.activateFileViewerSelectingURLs(&NSArray::from_slice(&[&*url]));
        }
    }

    /// The links recorded instead of opened, oldest first.
    pub fn opened_links(&self) -> Vec<String> {
        self.opened_links.borrow().clone()
    }
}

/// Expands `open-command` into argv tokens without invoking a shell.
fn expand_open_command(template: &str, target: &FileTarget) -> Vec<String> {
    let tokens = split_argv(template);
    if tokens.is_empty() {
        return Vec::new();
    }
    let file = target.path.to_string_lossy();
    let line = target.line.unwrap_or(1).to_string();
    let col = target.col.unwrap_or(1).to_string();
    let mut saw_file = false;
    let mut out: Vec<String> = tokens
        .into_iter()
        .map(|tok| {
            if tok.contains("{file}") {
                saw_file = true;
            }
            tok.replace("{file}", &file)
                .replace("{line}", &line)
                .replace("{col}", &col)
        })
        .collect();
    if !saw_file {
        out.push(file.into_owned());
    }
    out
}

fn split_argv(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut in_token = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {
                if c == '\\'
                    && let Some(next) = chars.next() {
                        cur.push(next);
                        continue;
                    }
                cur.push(c);
            }
            None if c.is_whitespace() => {
                if in_token {
                    args.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_token = true;
            }
            None => {
                in_token = true;
                if c == '\\'
                    && let Some(next) = chars.next() {
                        cur.push(next);
                        continue;
                    }
                cur.push(c);
            }
        }
    }
    if in_token {
        args.push(cur);
    }
    args
}

/// Bundles, scripts and executable files are revealed in Finder rather than
/// launched when Command-clicked without an explicit `open-command`.
fn should_reveal_only(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && matches!(
            ext.to_ascii_lowercase().as_str(),
            "app" | "command" | "tool" | "pkg" | "dmg" | "workflow"
        ) {
            return true;
        }
    if let Ok(meta) = std::fs::metadata(path)
        && meta.is_file() && (meta.permissions().mode() & 0o111) != 0 {
            return true;
        }
    false
}

/// What Command-click does with a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkKind {
    /// Opened in its default app.
    Open,
    /// Shown in the Finder rather than opened, which could run it.
    Reveal,
    Refused,
}

fn link_kind(url: &str) -> LinkKind {
    let scheme = url.split_once(':').map_or("", |(s, _)| s);
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" | "mailto" | "ftp" => LinkKind::Open,
        "file" => LinkKind::Reveal,
        _ => LinkKind::Refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_mail_and_file_links_open() {
        assert_eq!(link_kind("https://example.com/a:b"), LinkKind::Open);
        assert_eq!(link_kind("HTTP://example.com"), LinkKind::Open);
        assert_eq!(link_kind("mailto:a@b.c"), LinkKind::Open);
        assert_eq!(
            link_kind("file:///Applications/Calculator.app"),
            LinkKind::Reveal
        );
        assert_eq!(link_kind("x-custom://run"), LinkKind::Refused);
        assert_eq!(link_kind("javascript:alert(1)"), LinkKind::Refused);
        assert_eq!(link_kind("no scheme"), LinkKind::Refused);
    }

    #[test]
    fn open_command_expands_placeholders_without_shell() {
        let target = FileTarget {
            path: "/tmp/a b$c.rs".into(),
            line: Some(18),
            col: Some(4),
        };
        assert_eq!(
            expand_open_command("zed {file}:{line}:{col}", &target),
            vec!["zed", "/tmp/a b$c.rs:18:4"]
        );
        assert_eq!(
            expand_open_command("code -g", &target),
            vec!["code", "-g", "/tmp/a b$c.rs"]
        );
    }
}
