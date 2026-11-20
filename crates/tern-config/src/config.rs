//! The settings, their defaults, and how a config file's lines become them.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use tern_types::{CursorShape, Rgb, Theme};

use crate::bindings::Bindings;
use crate::colors::{compose, parse_color, Colors};
use crate::load::{default_path, expand_home, load_theme, read_file, Env};

/// Every setting, with defaults filled in. Each field is named after its
/// config key, except the colors and bold-is-bright, which are resolved into
/// [Config::theme], and keybind, which builds [Config::bindings].
#[derive(Clone, PartialEq, Debug)]
pub struct Config {
    /// font-family: the font's family name; empty for the system's
    /// monospaced font.
    pub font_family: String,
    /// font-size, in points, 6 to 72.
    pub font_size: f32,
    /// theme: the theme in use, as written (a name or a path). None when no
    /// theme is set or it could not be loaded.
    pub theme_name: Option<String>,
    /// The colors in effect: tern's defaults, then the theme's, then the
    /// color keys set directly in the config; and bold-is-bright.
    pub theme: Theme,
    /// cursor-style: Block, Beam (written "bar") or Underline.
    pub cursor_style: CursorShape,
    pub cursor_style_blink: bool,
    /// window-padding-x, in points.
    pub window_padding_x: u16,
    /// window-padding-y, in points.
    pub window_padding_y: u16,
    /// window-width of a new window, in cells: 10 to 1000.
    pub window_width: u16,
    /// window-height of a new window, in cells: 4 to 1000.
    pub window_height: u16,
    /// window-position-x of the first window, in points from the left edge
    /// of the main screen's visible area; a negative value counts from the
    /// right edge to the window's right edge. None: centered.
    pub window_position_x: Option<i32>,
    /// window-position-y, the same from the top edge (negative: from the
    /// bottom edge to the window's bottom edge).
    pub window_position_y: Option<i32>,
    /// scrollback-lines per pane: 0 to 1,000,000.
    pub scrollback_lines: u32,
    pub option_as_alt: OptionAsAlt,
    pub copy_on_select: bool,
    /// Ask before closing a pane whose foreground process is not the shell.
    pub confirm_close: bool,
    /// The command to run in new panes, exactly as written. None: the user's
    /// login shell.
    pub shell: Option<String>,
    pub working_directory: WorkingDirectory,
    /// Optional command run when Command-clicking a file path (`{file}`,
    /// `{line}`, `{col}` placeholders). None: open with macOS default handler.
    pub open_command: Option<String>,
    /// How much to dim panes without focus: 0 (not at all) to 1.
    pub unfocused_split_dim: f32,
    /// None: derive the divider color from the background.
    pub split_divider_color: Option<Rgb>,
    pub mouse_hide_while_typing: bool,
    pub quit_after_last_window_closed: bool,
    pub window_save_state: WindowSaveState,
    /// The key bindings: the defaults changed by every keybind line.
    pub bindings: Bindings,
}

/// window-save-state: whether open windows, tabs, split layouts and pane
/// working directories are saved to `~/.local/state/tern/session` and restored
/// at launch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowSaveState {
    Default,
    Always,
    Never,
}

/// option-as-alt: which Option keys act as Alt (Meta) instead of typing
/// special characters. "true" sets both, "false" neither, "left" or "right"
/// one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OptionAsAlt {
    pub left: bool,
    pub right: bool,
}

/// working-directory: where new tabs and splits start.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorkingDirectory {
    /// The home directory.
    Home,
    /// The focused pane's directory (home for the first window).
    Inherit,
    /// This absolute path; a leading ~ is already expanded.
    Path(PathBuf),
}

/// A problem with one setting. The setting was skipped; everything else
/// still applies.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    pub source: Source,
    /// The line in the source, from 1. 0 when the problem is with the whole
    /// source, such as a file that could not be read. For the command line,
    /// the position of the override, from 1.
    pub line: usize,
    /// What is wrong. A problem with a setting's value starts with its key,
    /// as in "font-size: expected a number ..."; a line that is not a
    /// setting at all, or a file that cannot be read, has no key to name.
    pub message: String,
}

/// Where a setting came from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Source {
    /// The text given to [Config::parse].
    Text,
    /// A config file or a theme file.
    File(PathBuf),
    /// The command-line overrides given to [Config::load].
    CommandLine,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.source {
            Source::Text => write!(f, "line {}: {}", self.line, self.message),
            Source::File(path) if self.line == 0 => {
                write!(f, "{}: {}", path.display(), self.message)
            }
            Source::File(path) => write!(f, "{}:{}: {}", path.display(), self.line, self.message),
            Source::CommandLine => write!(f, "command line: {}", self.message),
        }
    }
}

impl Default for Config {
    fn default() -> Config {
        Config {
            font_family: String::new(),
            font_size: 13.0,
            theme_name: None,
            theme: Theme::default(),
            cursor_style: CursorShape::Block,
            cursor_style_blink: false,
            window_padding_x: 10,
            window_padding_y: 4,
            window_width: 100,
            window_height: 30,
            window_position_x: None,
            window_position_y: None,
            scrollback_lines: 10_000,
            option_as_alt: OptionAsAlt {
                left: true,
                right: true,
            },
            copy_on_select: false,
            confirm_close: true,
            shell: None,
            working_directory: WorkingDirectory::Inherit,
            open_command: None,
            unfocused_split_dim: 0.25,
            split_divider_color: None,
            mouse_hide_while_typing: true,
            quit_after_last_window_closed: false,
            window_save_state: WindowSaveState::Default,
            bindings: Bindings::default(),
        }
    }
}

impl Config {
    /// Parses config text. Theme names are looked up beside the default
    /// config file, then among Ghostty's themes, so this reads theme files.
    pub fn parse(text: &str) -> (Config, Vec<Diagnostic>) {
        let env = Env::for_config(default_path().as_deref());
        let mut builder = Builder::new(&env);
        builder.feed(text.as_bytes(), &Source::Text);
        builder.finish()
    }

    /// Reads the config file at path, or at [default_path](crate::default_path)
    /// when path is None, then applies the command-line overrides (key and
    /// value pairs, the key without its leading --). A missing file is not an
    /// error. Theme names are looked up in a themes folder beside the config
    /// file, then among Ghostty's themes.
    pub fn load(
        path: Option<&Path>,
        cli_overrides: &[(String, String)],
    ) -> (Config, Vec<Diagnostic>) {
        let path = path.map(Path::to_path_buf).or_else(default_path);
        let env = Env::for_config(path.as_deref());
        let mut builder = Builder::new(&env);
        if let Some(path) = &path {
            let source = Source::File(path.clone());
            match read_file(path) {
                Ok(Some(bytes)) => builder.feed(&bytes, &source),
                Ok(None) => {}
                Err(message) => builder.report(&source, 0, message),
            }
        }
        builder.set_overrides(cli_overrides);
        builder.finish()
    }
}

/// The settings in a file: (line number, key and value) for each, or why the
/// line is not a setting. Blank lines and comments are skipped.
pub(crate) fn settings(
    bytes: &[u8],
) -> impl Iterator<Item = (usize, Result<(&str, &str), String>)> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    bytes
        .split(|&b| b == b'\n')
        .enumerate()
        .filter_map(|(index, raw)| {
            let line = index + 1;
            let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
            let Ok(text) = std::str::from_utf8(raw) else {
                return Some((line, Err("the line is not valid UTF-8".into())));
            };
            let text = text.trim();
            if text.is_empty() || text.starts_with('#') {
                return None;
            }
            let setting = match text.split_once('=') {
                None => Err(format!("expected key = value, got \"{text}\"")),
                Some((key, _)) if key.trim().is_empty() => {
                    Err(format!("missing key before = in \"{text}\""))
                }
                Some((key, value)) => Ok((key.trim(), unquote(value.trim()))),
            };
            Some((line, setting))
        })
}

/// The value without the double quotes around it, if it has them.
fn unquote(value: &str) -> &str {
    match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(inner) => inner,
        None => value,
    }
}

/// A theme setting, kept until every line is read: colors set directly
/// override the theme wherever they appear.
struct ThemeSetting {
    value: String,
    source: Source,
    line: usize,
}

/// Accumulates settings from the config text and the command line.
pub(crate) struct Builder<'a> {
    env: &'a Env,
    defaults: Config,
    config: Config,
    theme: Option<ThemeSetting>,
    colors: Colors,
    bold_is_bright: bool,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Builder<'a> {
    pub(crate) fn new(env: &'a Env) -> Builder<'a> {
        let defaults = Config::default();
        Builder {
            env,
            config: defaults.clone(),
            bold_is_bright: defaults.theme.bold_is_bright,
            defaults,
            theme: None,
            colors: Colors::default(),
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn feed(&mut self, bytes: &[u8], source: &Source) {
        for (line, setting) in settings(bytes) {
            match setting {
                Ok((key, value)) => self.set(key, value, source, line),
                Err(message) => self.report(source, line, message),
            }
        }
    }

    pub(crate) fn set_overrides(&mut self, overrides: &[(String, String)]) {
        for (index, (key, value)) in overrides.iter().enumerate() {
            self.set(
                key.trim(),
                unquote(value.trim()),
                &Source::CommandLine,
                index + 1,
            );
        }
    }

    pub(crate) fn report(&mut self, source: &Source, line: usize, message: String) {
        self.diagnostics.push(Diagnostic {
            source: source.clone(),
            line,
            message,
        });
    }

    fn set(&mut self, key: &str, value: &str, source: &Source, line: usize) {
        if let Err(message) = self.apply(key, value, source, line) {
            self.report(source, line, message);
        }
    }

    /// Applies one setting. An empty value restores the key's default.
    fn apply(&mut self, key: &str, v: &str, source: &Source, line: usize) -> Result<(), String> {
        if let Some(result) = self.colors.set(key, v) {
            return result.map_err(|message| format!("{key}: {message}"));
        }
        let d = &self.defaults;
        let c = &mut self.config;
        let result = match key {
            "font-family" => or_default(v, d.font_family.clone(), |v| no_nul(v).map(str::to_owned))
                .map(|x| c.font_family = x),
            "font-size" => {
                or_default(v, d.font_size, |v| number(v, 6.0, 72.0)).map(|x| c.font_size = x)
            }
            "theme" => {
                or_default(v, None, |v| no_nul(v).map(|v| Some(v.to_owned()))).map(|value| {
                    self.theme = value.map(|value| ThemeSetting {
                        value,
                        source: source.clone(),
                        line,
                    });
                })
            }
            "bold-is-bright" => {
                or_default(v, d.theme.bold_is_bright, boolean).map(|x| self.bold_is_bright = x)
            }
            "cursor-style" => {
                or_default(v, d.cursor_style, cursor_style).map(|x| c.cursor_style = x)
            }
            "cursor-style-blink" => {
                or_default(v, d.cursor_style_blink, boolean).map(|x| c.cursor_style_blink = x)
            }
            "window-padding-x" => or_default(v, d.window_padding_x, |v| whole(v, 0, u16::MAX))
                .map(|x| c.window_padding_x = x),
            "window-padding-y" => or_default(v, d.window_padding_y, |v| whole(v, 0, u16::MAX))
                .map(|x| c.window_padding_y = x),
            "window-width" => {
                or_default(v, d.window_width, |v| whole(v, 10, 1000)).map(|x| c.window_width = x)
            }
            "window-height" => {
                or_default(v, d.window_height, |v| whole(v, 4, 1000)).map(|x| c.window_height = x)
            }
            "window-position-x" => or_default(v, None, |v| {
                whole(v, -POSITION_LIMIT, POSITION_LIMIT).map(Some)
            })
            .map(|x| c.window_position_x = x),
            "window-position-y" => or_default(v, None, |v| {
                whole(v, -POSITION_LIMIT, POSITION_LIMIT).map(Some)
            })
            .map(|x| c.window_position_y = x),
            "scrollback-lines" => or_default(v, d.scrollback_lines, |v| whole(v, 0, 1_000_000))
                .map(|x| c.scrollback_lines = x),
            "option-as-alt" => {
                or_default(v, d.option_as_alt, option_as_alt).map(|x| c.option_as_alt = x)
            }
            "copy-on-select" => {
                or_default(v, d.copy_on_select, boolean).map(|x| c.copy_on_select = x)
            }
            "confirm-close" => or_default(v, d.confirm_close, boolean).map(|x| c.confirm_close = x),
            "shell" => {
                or_default(v, None, |v| no_nul(v).map(|v| Some(v.to_owned()))).map(|x| c.shell = x)
            }
            "working-directory" => {
                let home = self.env.home.as_deref();
                or_default(v, d.working_directory.clone(), |v| {
                    working_directory(v, home)
                })
                .map(|x| c.working_directory = x)
            }
            "open-command" => or_default(v, None, |v| no_nul(v).map(|v| Some(v.to_owned())))
                .map(|x| c.open_command = x),
            "unfocused-split-dim" => or_default(v, d.unfocused_split_dim, |v| number(v, 0.0, 1.0))
                .map(|x| c.unfocused_split_dim = x),
            "split-divider-color" => {
                or_default(v, d.split_divider_color, |v| parse_color(v).map(Some))
                    .map(|x| c.split_divider_color = x)
            }
            "mouse-hide-while-typing" => or_default(v, d.mouse_hide_while_typing, boolean)
                .map(|x| c.mouse_hide_while_typing = x),
            "quit-after-last-window-closed" => {
                or_default(v, d.quit_after_last_window_closed, boolean)
                    .map(|x| c.quit_after_last_window_closed = x)
            }
            "window-save-state" => or_default(v, d.window_save_state, window_save_state)
                .map(|x| c.window_save_state = x),
            "keybind" if v.is_empty() => {
                c.bindings = d.bindings.clone();
                Ok(())
            }
            "keybind" => c.bindings.apply(v),
            _ => return Err(format!("unknown key \"{key}\"")),
        };
        result.map_err(|message| format!("{key}: {message}"))
    }

    pub(crate) fn finish(mut self) -> (Config, Vec<Diagnostic>) {
        let mut theme_colors = None;
        if let Some(setting) = self.theme.take() {
            theme_colors = load_theme(
                &setting.value,
                &setting.source,
                setting.line,
                self.env,
                &mut self.diagnostics,
            );
            if theme_colors.is_some() {
                self.config.theme_name = Some(setting.value);
            }
        }
        self.config.theme = compose(theme_colors.as_ref(), &self.colors, self.bold_is_bright);
        (self.config, self.diagnostics)
    }
}

/// The largest window position offset accepted, in points: far beyond any
/// arrangement of screens, small enough that window math cannot overflow.
const POSITION_LIMIT: i32 = 100_000;

/// The default for an empty value, the parsed value otherwise.
fn or_default<T>(
    v: &str,
    default: T,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, String> {
    if v.is_empty() {
        Ok(default)
    } else {
        parse(v)
    }
}

/// A value passed on as text. The shell's path, the font's name and file
/// paths reach C APIs, which would cut them short at a NUL.
fn no_nul(v: &str) -> Result<&str, String> {
    if v.contains('\0') {
        Err("contains a NUL character".into())
    } else {
        Ok(v)
    }
}

fn boolean(v: &str) -> Result<bool, String> {
    match v {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("expected true or false, got \"{v}\"")),
    }
}

fn whole<T>(v: &str, min: T, max: T) -> Result<T, String>
where
    T: FromStr + PartialOrd + fmt::Display,
{
    v.parse::<T>()
        .ok()
        .filter(|n| *n >= min && *n <= max)
        .ok_or_else(|| format!("expected a whole number from {min} to {max}, got \"{v}\""))
}

fn number(v: &str, min: f32, max: f32) -> Result<f32, String> {
    v.parse::<f32>()
        .ok()
        .filter(|n| (min..=max).contains(n))
        .ok_or_else(|| format!("expected a number from {min} to {max}, got \"{v}\""))
}

fn cursor_style(v: &str) -> Result<CursorShape, String> {
    match v {
        "block" => Ok(CursorShape::Block),
        "bar" => Ok(CursorShape::Beam),
        "underline" => Ok(CursorShape::Underline),
        _ => Err(format!("expected block, bar or underline, got \"{v}\"")),
    }
}

fn option_as_alt(v: &str) -> Result<OptionAsAlt, String> {
    let (left, right) = match v {
        "true" => (true, true),
        "false" => (false, false),
        "left" => (true, false),
        "right" => (false, true),
        _ => return Err(format!("expected true, false, left or right, got \"{v}\"")),
    };
    Ok(OptionAsAlt { left, right })
}

fn working_directory(v: &str, home: Option<&Path>) -> Result<WorkingDirectory, String> {
    match no_nul(v)? {
        "home" => Ok(WorkingDirectory::Home),
        "inherit" => Ok(WorkingDirectory::Inherit),
        _ => {
            let path = expand_home(v, home)?;
            if path.is_absolute() {
                Ok(WorkingDirectory::Path(path))
            } else {
                Err(format!(
                    "expected home, inherit or an absolute path, got \"{v}\""
                ))
            }
        }
    }
}

fn window_save_state(v: &str) -> Result<WindowSaveState, String> {
    match v {
        "default" => Ok(WindowSaveState::Default),
        "always" => Ok(WindowSaveState::Always),
        "never" => Ok(WindowSaveState::Never),
        _ => Err(format!("expected default, always or never, got \"{v}\"")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::action::{Action, Direction};
    use tern_types::{Key, Mods};

    /// Every config key.
    pub(crate) const KEYS: &[&str] = &[
        "font-family",
        "font-size",
        "theme",
        "background",
        "foreground",
        "cursor-color",
        "cursor-text",
        "selection-background",
        "selection-foreground",
        "palette",
        "bold-is-bright",
        "cursor-style",
        "cursor-style-blink",
        "window-padding-x",
        "window-padding-y",
        "window-width",
        "window-height",
        "window-position-x",
        "window-position-y",
        "scrollback-lines",
        "option-as-alt",
        "copy-on-select",
        "confirm-close",
        "shell",
        "working-directory",
        "open-command",
        "unfocused-split-dim",
        "split-divider-color",
        "mouse-hide-while-typing",
        "quit-after-last-window-closed",
        "window-save-state",
        "keybind",
    ];

    /// An environment where theme names find nothing, so tests never read
    /// the machine's theme folders.
    fn env() -> Env {
        Env {
            home: Some(PathBuf::from("/Users/test")),
            theme_dirs: Vec::new(),
        }
    }

    pub(crate) fn parse_bytes(
        bytes: &[u8],
        overrides: &[(&str, &str)],
    ) -> (Config, Vec<Diagnostic>) {
        let env = env();
        let mut builder = Builder::new(&env);
        builder.feed(bytes, &Source::Text);
        let overrides: Vec<(String, String)> = overrides
            .iter()
            .map(|&(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        builder.set_overrides(&overrides);
        builder.finish()
    }

    pub(crate) fn parse(text: &str) -> (Config, Vec<Diagnostic>) {
        parse_bytes(text.as_bytes(), &[])
    }

    pub(crate) fn parse_clean(text: &str) -> Config {
        let (config, diagnostics) = parse(text);
        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
        config
    }

    fn lines(diagnostics: &[Diagnostic]) -> Vec<usize> {
        diagnostics.iter().map(|d| d.line).collect()
    }

    #[test]
    fn defaults_are_as_specified() {
        let c = Config::default();
        assert_eq!(c.font_family, "");
        assert_eq!(c.font_size, 13.0);
        assert_eq!(c.theme_name, None);
        assert_eq!(c.theme, Theme::default());
        assert!(!c.theme.bold_is_bright);
        assert_eq!(c.cursor_style, CursorShape::Block);
        assert!(!c.cursor_style_blink);
        assert_eq!((c.window_padding_x, c.window_padding_y), (10, 4));
        assert_eq!((c.window_width, c.window_height), (100, 30));
        assert_eq!(c.scrollback_lines, 10_000);
        assert_eq!(
            c.option_as_alt,
            OptionAsAlt {
                left: true,
                right: true
            }
        );
        assert!(!c.copy_on_select);
        assert!(c.confirm_close);
        assert_eq!(c.shell, None);
        assert_eq!(c.working_directory, WorkingDirectory::Inherit);
        assert_eq!(c.unfocused_split_dim, 0.25);
        assert_eq!(c.split_divider_color, None);
        assert!(c.mouse_hide_while_typing);
        assert!(!c.quit_after_last_window_closed);
        assert_eq!(c.bindings, Bindings::default());
        assert_eq!(parse_clean(""), c);
    }

    #[test]
    fn every_key_takes_good_values() {
        let c = parse_clean(
            "font-family = JetBrains Mono
             font-size = 14.5
             background = #000000
             foreground = ffffff
             cursor-color = #ff0000
             cursor-text = #00ff00
             selection-background = #0000ff
             selection-foreground = #ffff00
             palette = 1=#111111
             palette = 2=#222222
             bold-is-bright = true
             cursor-style = bar
             cursor-style-blink = true
             window-padding-x = 0
             window-padding-y = 20
             window-width = 10
             window-height = 4
             scrollback-lines = 0
             option-as-alt = left
             copy-on-select = true
             confirm-close = false
             shell = /opt/homebrew/bin/fish -l
             working-directory = ~/src
             unfocused-split-dim = 1
             split-divider-color = #333
             mouse-hide-while-typing = false
             quit-after-last-window-closed = true
             keybind = cmd+j=new_tab",
        );
        assert_eq!(c.font_family, "JetBrains Mono");
        assert_eq!(c.font_size, 14.5);
        assert_eq!(c.theme.bg, 0x000000);
        assert_eq!(c.theme.fg, 0xffffff);
        assert_eq!(c.theme.cursor, Some(0xff0000));
        assert_eq!(c.theme.cursor_text, Some(0x00ff00));
        assert_eq!(c.theme.selection_bg, Some(0x0000ff));
        assert_eq!(c.theme.selection_fg, Some(0xffff00));
        assert_eq!(c.theme.palette[1], 0x111111);
        assert_eq!(c.theme.palette[2], 0x222222);
        assert!(c.theme.bold_is_bright);
        assert_eq!(c.cursor_style, CursorShape::Beam);
        assert!(c.cursor_style_blink);
        assert_eq!((c.window_padding_x, c.window_padding_y), (0, 20));
        assert_eq!((c.window_width, c.window_height), (10, 4));
        assert_eq!(c.scrollback_lines, 0);
        assert_eq!(
            c.option_as_alt,
            OptionAsAlt {
                left: true,
                right: false
            }
        );
        assert!(c.copy_on_select);
        assert!(!c.confirm_close);
        assert_eq!(c.shell.as_deref(), Some("/opt/homebrew/bin/fish -l"));
        assert_eq!(
            c.working_directory,
            WorkingDirectory::Path("/Users/test/src".into())
        );
        assert_eq!(c.unfocused_split_dim, 1.0);
        assert_eq!(c.split_divider_color, Some(0x333333));
        assert!(!c.mouse_hide_while_typing);
        assert!(c.quit_after_last_window_closed);
        assert_eq!(
            c.bindings.lookup(Key::Char('j'), Mods::SUPER),
            Some(&Action::NewTab)
        );
    }

    #[test]
    fn window_position() {
        let c = Config::default();
        assert_eq!((c.window_position_x, c.window_position_y), (None, None));
        let c = parse_clean("window-position-x = 40\nwindow-position-y = -20");
        assert_eq!(
            (c.window_position_x, c.window_position_y),
            (Some(40), Some(-20))
        );
        let c = parse_clean("window-position-x = 40\nwindow-position-x =");
        assert_eq!(c.window_position_x, None);
        for bad in ["1.5", "left", "100001", "-100001"] {
            let (c, diagnostics) = parse(&format!("window-position-y = {bad}"));
            assert_eq!(c.window_position_y, None, "{bad}");
            assert!(
                diagnostics[0].message.starts_with("window-position-y: "),
                "{bad}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn more_good_values() {
        let c = parse_clean("font-size = 6\nwindow-width = 1000\nwindow-height = 1000\nscrollback-lines = 1000000\nunfocused-split-dim = 0");
        assert_eq!(c.font_size, 6.0);
        assert_eq!((c.window_width, c.window_height), (1000, 1000));
        assert_eq!(c.scrollback_lines, 1_000_000);
        assert_eq!(c.unfocused_split_dim, 0.0);
        assert_eq!(parse_clean("font-size = 72").font_size, 72.0);
        assert_eq!(
            parse_clean("cursor-style = underline").cursor_style,
            CursorShape::Underline
        );
        assert_eq!(
            parse_clean("cursor-style = bar\ncursor-style = block").cursor_style,
            CursorShape::Block
        );
        assert_eq!(
            parse_clean("option-as-alt = right").option_as_alt,
            OptionAsAlt {
                left: false,
                right: true
            }
        );
        assert_eq!(
            parse_clean("option-as-alt = false").option_as_alt,
            OptionAsAlt {
                left: false,
                right: false
            }
        );
        assert_eq!(
            parse_clean("option-as-alt = true").option_as_alt,
            OptionAsAlt {
                left: true,
                right: true
            }
        );
        assert_eq!(
            parse_clean("working-directory = home").working_directory,
            WorkingDirectory::Home
        );
        assert_eq!(
            parse_clean("working-directory = inherit").working_directory,
            WorkingDirectory::Inherit
        );
        assert_eq!(
            parse_clean("working-directory = /tmp/x y").working_directory,
            WorkingDirectory::Path("/tmp/x y".into())
        );
        assert_eq!(
            parse_clean("working-directory = ~").working_directory,
            WorkingDirectory::Path("/Users/test".into())
        );
        assert_eq!(
            parse_clean("window-padding-x = 65535").window_padding_x,
            u16::MAX
        );
    }

    #[test]
    fn bad_values_are_reported_and_skipped() {
        let cases = [
            ("font-size", "5.9", "from 6 to 72"),
            ("font-size", "73", "from 6 to 72"),
            ("font-size", "big", "from 6 to 72"),
            ("font-size", "NaN", "from 6 to 72"),
            ("font-size", "inf", "from 6 to 72"),
            ("background", "#12345", "expected a color"),
            ("foreground", "white", "expected a color"),
            ("cursor-color", "#ggg", "expected a color"),
            ("cursor-text", "123", "expected a color"),
            ("selection-background", "#1234567", "expected a color"),
            ("selection-foreground", "rgb(1,2,3)", "expected a color"),
            ("palette", "256=#000000", "from 0 to 255"),
            ("palette", "#000000", "expected N=#rrggbb"),
            ("palette", "x=#000000", "from 0 to 255"),
            ("palette", "1", "expected N=#rrggbb"),
            ("palette", "1=#zzzzzz", "expected a color"),
            ("bold-is-bright", "yes", "expected true or false"),
            ("cursor-style", "beam", "expected block, bar or underline"),
            ("cursor-style-blink", "1", "expected true or false"),
            ("window-padding-x", "-1", "from 0 to 65535"),
            ("window-padding-y", "1.5", "from 0 to 65535"),
            ("window-padding-y", "70000", "from 0 to 65535"),
            ("window-width", "9", "from 10 to 1000"),
            ("window-width", "1001", "from 10 to 1000"),
            ("window-height", "3", "from 4 to 1000"),
            ("window-height", "tall", "from 4 to 1000"),
            ("scrollback-lines", "1000001", "from 0 to 1000000"),
            ("scrollback-lines", "-5", "from 0 to 1000000"),
            ("scrollback-lines", "10_000", "from 0 to 1000000"),
            (
                "option-as-alt",
                "both",
                "expected true, false, left or right",
            ),
            ("copy-on-select", "TRUE", "expected true or false"),
            ("confirm-close", "no", "expected true or false"),
            ("working-directory", "relative/dir", "absolute path"),
            ("working-directory", "~bob/src", "only ~ and ~/"),
            ("unfocused-split-dim", "1.5", "from 0 to 1"),
            ("unfocused-split-dim", "-0.1", "from 0 to 1"),
            ("split-divider-color", "grey", "expected a color"),
            ("mouse-hide-while-typing", "on", "expected true or false"),
            (
                "quit-after-last-window-closed",
                "2",
                "expected true or false",
            ),
            ("keybind", "cmd+x", "expected <trigger>=<action>"),
            ("keybind", "cmd+x=frobnicate", "unknown action"),
            ("keybind", "meta+x=new_tab", "unknown modifier"),
            (
                "theme",
                "some/where",
                "neither a theme name nor an absolute path",
            ),
            ("theme", "NoSuchTheme", "not found"),
        ];
        for (key, value, message) in cases {
            let text = format!("# comment\n{key} = {value}\n");
            let (config, diagnostics) = parse(&text);
            assert_eq!(
                config,
                Config::default(),
                "{key} = {value} changed the config"
            );
            assert_eq!(diagnostics.len(), 1, "{key} = {value}: {diagnostics:#?}");
            let d = &diagnostics[0];
            assert_eq!((&d.source, d.line), (&Source::Text, 2), "{key} = {value}");
            assert!(d.message.starts_with(&format!("{key}: ")), "{}", d.message);
            assert!(
                d.message.contains(message),
                "{key} = {value}: {}",
                d.message
            );
        }
    }

    #[test]
    fn unknown_keys_and_malformed_lines_are_reported() {
        let (config, diagnostics) =
            parse("font-size = 14\nfont-sise = 15\nno equals sign\n= 3\nFont-Size = 16\n");
        assert_eq!(config.font_size, 14.0);
        let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "unknown key \"font-sise\"",
                "expected key = value, got \"no equals sign\"",
                "missing key before = in \"= 3\"",
                "unknown key \"Font-Size\"",
            ]
        );
        assert_eq!(lines(&diagnostics), [2, 3, 4, 5]);
    }

    #[test]
    fn comments_blank_lines_whitespace_and_quotes() {
        let text = "\u{feff}# leading comment\r\n\r\n   # indented comment\n\tfont-size\t=\t15  \r\nfont-family = \"Fira Code\"\nshell=\"  spaced  \"\nkeybind = \"cmd+y=text:a \"\nforeground = #abcdef # not a comment\nbackground = \"#123456\"\n";
        let (c, diagnostics) = parse(text);
        assert_eq!(c.font_size, 15.0);
        assert_eq!(c.font_family, "Fira Code");
        assert_eq!(c.shell.as_deref(), Some("  spaced  "));
        assert_eq!(
            c.bindings.lookup(Key::Char('y'), Mods::SUPER),
            Some(&Action::Text(b"a ".to_vec()))
        );
        assert_eq!(c.theme.bg, 0x123456);
        // Comments start only at the beginning of a line, since # starts colors.
        assert_eq!(lines(&diagnostics), [8]);
        assert!(diagnostics[0]
            .message
            .starts_with("foreground: expected a color"));
        assert_eq!(c.theme.fg, Theme::default().fg);
    }

    #[test]
    fn a_lone_quote_is_kept() {
        assert_eq!(parse_clean("font-family = \"").font_family, "\"");
        assert_eq!(parse_clean("font-family = \"Menlo").font_family, "\"Menlo");
        assert_eq!(parse_clean("font-family = a \"b\"").font_family, "a \"b\"");
    }

    #[test]
    fn invalid_utf8_lines_are_reported_and_skipped() {
        let (c, diagnostics) = parse_bytes(
            b"font-size = 15\nfont-family = \xff\xfe\nwindow-width = 80\n",
            &[],
        );
        assert_eq!(c.font_size, 15.0);
        assert_eq!(c.window_width, 80);
        assert_eq!(c.font_family, "");
        assert_eq!(lines(&diagnostics), [2]);
        assert_eq!(diagnostics[0].message, "the line is not valid UTF-8");
    }

    #[test]
    fn later_settings_win() {
        let c = parse_clean(
            "font-size = 10\nfont-size = 11\nbackground = #111111\nbackground = #222222",
        );
        assert_eq!(c.font_size, 11.0);
        assert_eq!(c.theme.bg, 0x222222);
    }

    #[test]
    fn an_empty_value_restores_the_default() {
        let set = "font-family = X\nfont-size = 20\nbackground = #010101\nforeground = #020202\ncursor-color = #030303\ncursor-text = #040404\nselection-background = #050505\nselection-foreground = #060606\npalette = 4=#070707\nbold-is-bright = true\ncursor-style = bar\ncursor-style-blink = true\nwindow-padding-x = 1\nwindow-padding-y = 1\nwindow-width = 11\nwindow-height = 5\nscrollback-lines = 1\noption-as-alt = false\ncopy-on-select = true\nconfirm-close = false\nshell = /bin/sh\nworking-directory = home\nopen-command = zed\nunfocused-split-dim = 0.5\nsplit-divider-color = #080808\nmouse-hide-while-typing = false\nquit-after-last-window-closed = true\nwindow-save-state = never\nkeybind = clear\n";
        assert_ne!(parse_clean(set), Config::default());
        let mut text = set.to_owned();
        for key in KEYS {
            text.push_str(&format!("{key} =\n"));
        }
        assert_eq!(parse_clean(&text), Config::default());
        // Quotes around nothing are an empty value too.
        assert_eq!(
            parse_clean("font-size = 20\nfont-size = \"\""),
            Config::default()
        );
    }

    #[test]
    fn every_key_is_known() {
        for key in KEYS {
            let (_, diagnostics) = parse(&format!("{key} ="));
            assert!(diagnostics.is_empty(), "{key}: {diagnostics:#?}");
        }
    }

    #[test]
    fn diagnostics_carry_line_numbers() {
        let text = "font-size = 14\n\n# comment\nfont-size = 99\nwindow-width = 80\nbogus = 1\n\n\ncursor-style = beam\n";
        let (c, diagnostics) = parse(text);
        assert_eq!(lines(&diagnostics), [4, 6, 9]);
        assert!(diagnostics.iter().all(|d| d.source == Source::Text));
        assert_eq!(c.font_size, 14.0);
        assert_eq!(c.window_width, 80);
    }

    #[test]
    fn command_line_overrides_the_text() {
        let text =
            "font-size = 14\nwindow-width = 80\nkeybind = cmd+j=new_tab\npalette = 1=#111111";
        let (c, diagnostics) = parse_bytes(
            text.as_bytes(),
            &[
                ("font-size", "16"),
                ("keybind", "cmd+k=split:down"),
                ("palette", "2=#222222"),
                (" shell ", " \"/bin/zsh\" "),
                ("window-width", "5"),
                ("nope", "1"),
            ],
        );
        assert_eq!(c.font_size, 16.0);
        // Rejected overrides leave the file's value.
        assert_eq!(c.window_width, 80);
        // List-valued keys accumulate.
        assert_eq!(
            c.bindings.lookup(Key::Char('j'), Mods::SUPER),
            Some(&Action::NewTab)
        );
        assert_eq!(
            c.bindings.lookup(Key::Char('k'), Mods::SUPER),
            Some(&Action::Split(Direction::Down))
        );
        assert_eq!(
            (c.theme.palette[1], c.theme.palette[2]),
            (0x111111, 0x222222)
        );
        assert_eq!(c.shell.as_deref(), Some("/bin/zsh"));
        let found: Vec<(Source, usize)> = diagnostics
            .iter()
            .map(|d| (d.source.clone(), d.line))
            .collect();
        assert_eq!(found, [(Source::CommandLine, 5), (Source::CommandLine, 6)]);
        assert!(diagnostics[0].message.starts_with("window-width: "));
        assert_eq!(diagnostics[1].message, "unknown key \"nope\"");
    }

    #[test]
    fn command_line_empty_value_unsets() {
        let (c, diagnostics) = parse_bytes(
            b"shell = /bin/zsh\ntheme = Missing\n",
            &[("shell", ""), ("theme", "")],
        );
        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn missing_theme_is_reported_at_its_line_and_colors_still_apply() {
        let (c, diagnostics) = parse("background = #101010\n\ntheme = Nowhere\n");
        assert_eq!(c.theme_name, None);
        assert_eq!(c.theme.bg, 0x101010);
        assert_eq!(lines(&diagnostics), [3]);
        assert!(diagnostics[0]
            .message
            .starts_with("theme: \"Nowhere\" not found"));
    }

    #[test]
    fn keybind_clear_and_unbind() {
        let c = parse_clean("keybind = clear\nkeybind = cmd+t=new_tab");
        assert_eq!(c.bindings.len(), 1);
        assert_eq!(c.bindings.lookup(Key::Char('w'), Mods::SUPER), None);
        let c = parse_clean("keybind = cmd+w=unbind");
        assert_eq!(c.bindings.lookup(Key::Char('w'), Mods::SUPER), None);
        assert_eq!(c.bindings.len(), Bindings::default().len() - 1);
        // An empty keybind brings the defaults back.
        assert_eq!(
            parse_clean("keybind = clear\nkeybind =").bindings,
            Bindings::default()
        );
    }

    #[test]
    fn diagnostic_display() {
        let d = |source, line| Diagnostic {
            source,
            line,
            message: "font-size: bad".into(),
        };
        assert_eq!(d(Source::Text, 3).to_string(), "line 3: font-size: bad");
        assert_eq!(
            d(Source::File("/c/config".into()), 3).to_string(),
            "/c/config:3: font-size: bad"
        );
        assert_eq!(
            d(Source::File("/c/config".into()), 0).to_string(),
            "/c/config: font-size: bad"
        );
        assert_eq!(
            d(Source::CommandLine, 2).to_string(),
            "command line: font-size: bad"
        );
    }

    #[test]
    fn public_parse_needs_no_files_without_a_theme() {
        let (c, diagnostics) = Config::parse("font-size = 18\nbogus = 1");
        assert_eq!(c.font_size, 18.0);
        assert_eq!(lines(&diagnostics), [2]);
    }
    #[test]
    fn settings_can_cross_threads() {
        // The app may load or reload the config off the main thread.
        fn sendable<T: Send + Sync + 'static>() {}
        sendable::<Config>();
        sendable::<Diagnostic>();
        sendable::<Bindings>();
        sendable::<Action>();
    }
    #[test]
    fn nul_characters_are_refused_in_strings() {
        // These strings reach C APIs that would cut them short at the NUL.
        let (c, diagnostics) = parse(
            "font-family = Me\0nlo\nshell = /bin/zsh\0 -l\nworking-directory = /tmp/\0x\ntheme = A\0B\n",
        );
        assert_eq!(c, Config::default());
        assert_eq!(lines(&diagnostics), [1, 2, 3, 4]);
        for (d, key) in
            diagnostics
                .iter()
                .zip(["font-family", "shell", "working-directory", "theme"])
        {
            assert_eq!(d.message, format!("{key}: contains a NUL character"));
        }
        // Escaped NULs in text actions are bytes for the terminal, not C strings.
        let c = parse_clean(r"keybind = cmd+y=text:\x00");
        assert_eq!(
            c.bindings.lookup(Key::Char('y'), Mods::SUPER),
            Some(&Action::Text(vec![0]))
        );
    }
}
