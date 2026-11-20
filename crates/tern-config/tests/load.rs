//! Config::load against real files: the config file, theme folders, and
//! Ghostty's bundled themes when Ghostty is installed.

use std::fs;
use std::path::{Path, PathBuf};

use tern_config::{Config, Diagnostic, Source};

const GHOSTTY_THEMES: &str = "/Applications/Ghostty.app/Contents/Resources/ghostty/themes";

/// A fresh directory in Cargo's scratch space for integration tests,
/// removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("tern-config-{name}-{}", std::process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    fn write(&self, relative: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn overrides(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|&(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn located(diagnostics: &[Diagnostic]) -> Vec<(Source, usize)> {
    diagnostics
        .iter()
        .map(|d| (d.source.clone(), d.line))
        .collect()
}

#[test]
fn a_missing_file_gives_the_defaults() {
    let scratch = Scratch::new("missing");
    let (config, diagnostics) = Config::load(Some(&scratch.path("config")), &[]);
    assert_eq!(config, Config::default());
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}

#[test]
fn the_file_applies_and_the_command_line_overrides_it() {
    let scratch = Scratch::new("file");
    let path = scratch.write(
        "config",
        "font-size = 15\nwindow-width = 120\nbogus = 1\nkeybind = cmd+j=new_tab\n",
    );
    let (config, diagnostics) = Config::load(
        Some(&path),
        &overrides(&[
            ("window-width", "90"),
            ("keybind", "cmd+k=new_window"),
            ("font-size", "200"),
        ]),
    );
    assert_eq!(config.font_size, 15.0);
    assert_eq!(config.window_width, 90);
    let bound = |c| {
        config
            .bindings
            .lookup(tern_types::Key::Char(c), tern_types::Mods::SUPER)
    };
    assert_eq!(bound('j'), Some(&tern_config::Action::NewTab));
    assert_eq!(bound('k'), Some(&tern_config::Action::NewWindow));
    assert_eq!(
        located(&diagnostics),
        [(Source::File(path.clone()), 3), (Source::CommandLine, 3)]
    );
    assert_eq!(
        diagnostics[0].to_string(),
        format!("{}:3: unknown key \"bogus\"", path.display())
    );
}

#[test]
fn an_unreadable_config_is_reported_and_the_command_line_still_applies() {
    let scratch = Scratch::new("unreadable");
    let dir = scratch.path("config");
    fs::create_dir_all(&dir).unwrap();
    let (config, diagnostics) = Config::load(Some(&dir), &overrides(&[("font-size", "20")]));
    assert_eq!(config.font_size, 20.0);
    assert_eq!(located(&diagnostics), [(Source::File(dir), 0)]);
    assert_eq!(diagnostics[0].message, "is not a regular file");
}

#[test]
fn an_oversized_config_is_refused() {
    let scratch = Scratch::new("oversized");
    let mut text = "font-size = 20\n".to_owned();
    text.push_str(&"# padding\n".repeat(110_000));
    let path = scratch.write("config", text);
    let (config, diagnostics) = Config::load(Some(&path), &[]);
    assert_eq!(config, Config::default());
    assert_eq!(located(&diagnostics), [(Source::File(path), 0)]);
    assert_eq!(diagnostics[0].message, "is larger than 1024 KiB");
}

#[test]
fn bad_bytes_spoil_only_their_line() {
    let scratch = Scratch::new("utf8");
    let path = scratch.write(
        "config",
        b"font-size = 15\r\nfont-family = \xc3\x28\r\nwindow-height = 40\r\n",
    );
    let (config, diagnostics) = Config::load(Some(&path), &[]);
    assert_eq!((config.font_size, config.window_height), (15.0, 40));
    assert_eq!(config.font_family, "");
    assert_eq!(located(&diagnostics), [(Source::File(path), 2)]);
}

#[test]
fn a_theme_name_is_found_beside_the_config_and_direct_colors_win() {
    let scratch = Scratch::new("theme-name");
    scratch.write(
        "themes/Paper",
        "background = #fafafa\nforeground = #202020\ncursor-color = #ff0000\npalette = 1=#aa0000\npalette = 2=#00aa00\n",
    );
    // The direct color comes before the theme line and still wins.
    let path = scratch.write(
        "config",
        "palette = 2=#123456\ntheme = Paper\nbold-is-bright = true\n",
    );
    let (config, diagnostics) = Config::load(Some(&path), &[]);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(config.theme_name.as_deref(), Some("Paper"));
    let theme = &config.theme;
    assert_eq!(
        (theme.bg, theme.fg, theme.cursor),
        (0xfafafa, 0x202020, Some(0xff0000))
    );
    assert_eq!((theme.palette[1], theme.palette[2]), (0xaa0000, 0x123456));
    assert!(theme.bold_is_bright);
    // What the theme leaves out follows the text rather than tern's defaults.
    assert_eq!(
        (theme.cursor_text, theme.selection_bg, theme.selection_fg),
        (None, None, None)
    );

    // The command line can override both.
    let (config, diagnostics) = Config::load(
        Some(&path),
        &overrides(&[("background", "#000000"), ("theme", "")]),
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(config.theme_name, None);
    assert_eq!(config.theme.bg, 0x000000);
    assert_eq!(config.theme.fg, tern_types::Theme::default().fg);
    assert_eq!(config.theme.palette[2], 0x123456);
}

#[test]
fn a_theme_by_absolute_path() {
    let scratch = Scratch::new("theme-path");
    let theme = scratch.write("elsewhere/My Theme", "background = #010203\n");
    let path = scratch.write("config", format!("theme = \"{}\"\n", theme.display()));
    let (config, diagnostics) = Config::load(Some(&path), &[]);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(config.theme.bg, 0x010203);
    assert_eq!(config.theme_name, Some(theme.display().to_string()));
}

#[test]
fn problems_inside_a_theme_name_the_theme_file() {
    let scratch = Scratch::new("theme-problems");
    let theme = scratch.write(
        "themes/Broken",
        "background = #010101\nfont-size = 30\nforeground = nope\n",
    );
    let path = scratch.write("config", "\ntheme = Broken\n");
    let (config, diagnostics) = Config::load(Some(&path), &[]);
    assert_eq!(config.theme_name.as_deref(), Some("Broken"));
    assert_eq!(config.theme.bg, 0x010101);
    assert_eq!(config.font_size, 13.0);
    assert_eq!(
        located(&diagnostics),
        [(Source::File(theme.clone()), 2), (Source::File(theme), 3)]
    );
    assert!(diagnostics[0].message.contains("cannot be set in a theme"));
    assert!(diagnostics[1]
        .message
        .starts_with("foreground: expected a color"));
}

#[test]
fn a_missing_theme_is_reported_where_it_was_set() {
    let scratch = Scratch::new("theme-missing");
    let path = scratch.write(
        "config",
        "background = #111111\ntheme = No Such Theme 12345\n",
    );
    let (config, diagnostics) = Config::load(Some(&path), &[]);
    assert_eq!(config.theme_name, None);
    assert_eq!(config.theme.bg, 0x111111);
    assert_eq!(located(&diagnostics), [(Source::File(path), 2)]);
    let message = &diagnostics[0].message;
    assert!(
        message.starts_with("theme: \"No Such Theme 12345\" not found (looked for "),
        "{message}"
    );
    assert!(message.contains(
        &scratch
            .path("themes/No Such Theme 12345")
            .display()
            .to_string()
    ));
    assert!(message.contains(GHOSTTY_THEMES));

    // From the command line, the diagnostic says so.
    let (_, diagnostics) = Config::load(
        Some(&scratch.path("absent")),
        &overrides(&[("theme", "/no/such/file")]),
    );
    assert_eq!(located(&diagnostics), [(Source::CommandLine, 1)]);

    // A theme path that is a folder.
    let folder = scratch.path("themes/Folder");
    fs::create_dir_all(&folder).unwrap();
    let (_, diagnostics) = Config::load(
        Some(&scratch.path("absent")),
        &overrides(&[("theme", "Folder")]),
    );
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].message,
        format!("theme: {}: is not a regular file", folder.display())
    );
}

#[test]
fn tern_themes_come_before_ghosttys() {
    let scratch = Scratch::new("theme-order");
    scratch.write("themes/Dracula", "background = #abcdef\n");
    let (config, diagnostics) = Config::load(
        Some(&scratch.path("config")),
        &overrides(&[("theme", "Dracula")]),
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(config.theme.bg, 0xabcdef);
}

#[test]
fn ghosttys_bundled_themes_load_cleanly() {
    let Ok(entries) = fs::read_dir(GHOSTTY_THEMES) else {
        eprintln!("Ghostty is not installed; skipping");
        return;
    };
    let scratch = Scratch::new("ghostty");
    let config = scratch.path("config");
    let mut count = 0;
    for entry in entries {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let (loaded, diagnostics) = Config::load(Some(&config), &overrides(&[("theme", &name)]));
        assert!(diagnostics.is_empty(), "{name}: {diagnostics:#?}");
        assert_eq!(loaded.theme_name.as_deref(), Some(name.as_str()));
        count += 1;
    }
    assert!(count > 0);

    // One theme checked value by value against its file.
    let dracula = Path::new(GHOSTTY_THEMES).join("Dracula");
    if dracula.exists() {
        let (loaded, _) = Config::load(Some(&config), &overrides(&[("theme", "Dracula")]));
        let text = fs::read_to_string(&dracula).unwrap();
        let theme = &loaded.theme;
        let mut checked = 0;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let (expected, actual) = match key {
                "palette" => {
                    let (index, color) = value.split_once('=').unwrap();
                    (color, theme.palette[index.parse::<usize>().unwrap()])
                }
                "background" => (value, theme.bg),
                "foreground" => (value, theme.fg),
                "cursor-color" => (value, theme.cursor.unwrap()),
                "cursor-text" => (value, theme.cursor_text.unwrap()),
                "selection-background" => (value, theme.selection_bg.unwrap()),
                "selection-foreground" => (value, theme.selection_fg.unwrap()),
                _ => continue,
            };
            let expected = u32::from_str_radix(expected.trim_start_matches('#'), 16).unwrap();
            assert_eq!(actual, expected, "{line}");
            checked += 1;
        }
        assert!(checked >= 22, "only {checked} Dracula colors checked");
    }
}

#[test]
fn the_default_path_is_used_when_none_is_given() {
    // The user's own config may exist and hold anything; loading still
    // works and the command line still applies.
    let (config, diagnostics) =
        Config::load(None, &overrides(&[("bogus", "1"), ("window-height", "33")]));
    assert_eq!(config.window_height, 33);
    assert!(diagnostics
        .iter()
        .any(|d| d.source == Source::CommandLine && d.message == "unknown key \"bogus\""));
    assert!(diagnostics.iter().all(|d| d.source != Source::Text));
}

/// Loads on another thread and fails, rather than hangs, if loading blocks.
fn load_within_seconds(path: &Path, cli: &[(&str, &str)]) -> (Config, Vec<Diagnostic>) {
    let (path, cli) = (path.to_path_buf(), overrides(cli));
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(Config::load(Some(&path), &cli));
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("loading blocked")
}

#[test]
fn fifos_are_refused_without_being_opened() {
    let scratch = Scratch::new("fifo");
    let config = scratch.path("config");
    let theme = scratch.path("themes/Pipe");
    fs::create_dir_all(scratch.path("themes")).unwrap();
    for fifo in [&config, &theme] {
        let status = std::process::Command::new("/usr/bin/mkfifo")
            .arg(fifo)
            .status()
            .unwrap();
        assert!(status.success());
    }
    // Opening a FIFO for reading blocks until a writer appears.
    let (config_loaded, diagnostics) = load_within_seconds(&config, &[("theme", "Pipe")]);
    assert_eq!(config_loaded, Config::default());
    let messages: Vec<String> = diagnostics.iter().map(|d| d.to_string()).collect();
    assert_eq!(
        messages,
        [
            format!("{}: is not a regular file", config.display()),
            format!(
                "command line: theme: {}: is not a regular file",
                theme.display()
            ),
        ]
    );
}

#[test]
fn symlinks_are_followed() {
    let scratch = Scratch::new("symlink");
    let real = scratch.write("dotfiles/tern", "font-size = 17\ntheme = Linked\n");
    let real_theme = scratch.write("dotfiles/linked-theme", "background = #0a0b0c\n");
    fs::create_dir_all(scratch.path("conf/themes")).unwrap();
    let config = scratch.path("conf/config");
    std::os::unix::fs::symlink(&real, &config).unwrap();
    std::os::unix::fs::symlink(&real_theme, scratch.path("conf/themes/Linked")).unwrap();
    let (loaded, diagnostics) = Config::load(Some(&config), &[]);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(loaded.font_size, 17.0);
    // Theme names are looked up beside the path given, not the link's target.
    assert_eq!(loaded.theme.bg, 0x0a0b0c);

    // A dangling link is a missing file.
    let dangling = scratch.path("conf/dangling");
    std::os::unix::fs::symlink(scratch.path("nowhere"), &dangling).unwrap();
    assert_eq!(
        Config::load(Some(&dangling), &[]),
        (Config::default(), vec![])
    );
}

#[test]
fn an_unreadable_file_is_reported() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("permissions");
    let path = scratch.write("config", "font-size = 20\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&path).is_ok() {
        eprintln!("running with permission to read anything; skipping");
        return;
    }
    let (config, diagnostics) = Config::load(Some(&path), &[]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(config, Config::default());
    assert_eq!(located(&diagnostics), [(Source::File(path), 0)]);
    assert!(
        diagnostics[0].message.starts_with("cannot be read: "),
        "{}",
        diagnostics[0].message
    );
}

#[test]
fn a_config_of_exactly_one_mebibyte_is_read() {
    let scratch = Scratch::new("limit");
    let mut text = "font-size = 20\n".to_owned();
    while text.len() < (1 << 20) {
        text.push('#');
    }
    let path = scratch.write("config", &text);
    let (config, diagnostics) = Config::load(Some(&path), &[]);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(config.font_size, 20.0);

    text.push('#');
    let path = scratch.write("config", &text);
    let (config, diagnostics) = Config::load(Some(&path), &[]);
    assert_eq!(config.font_size, 13.0);
    assert_eq!(diagnostics[0].message, "is larger than 1024 KiB");
}
