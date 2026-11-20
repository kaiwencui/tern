//! Everything that touches the system: finding and reading the config file
//! and theme files, and the command line.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::colors::Colors;
use crate::config::{settings, Diagnostic, Source};

/// Ghostty's bundled themes, searched for theme names not found in tern's own
/// themes folder.
const GHOSTTY_THEMES: &str = "/Applications/Ghostty.app/Contents/Resources/ghostty/themes";

/// Config and theme files are a few kilobytes; anything bigger is a wrong
/// path (a log, a binary), not configuration.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// What resolving paths needs from the system.
pub(crate) struct Env {
    /// For ~ in paths.
    pub(crate) home: Option<PathBuf>,
    /// Where theme names are looked up, in order.
    pub(crate) theme_dirs: Vec<PathBuf>,
}

impl Env {
    /// The real environment, with theme names looked up in the themes folder
    /// beside the config file, then among Ghostty's.
    pub(crate) fn for_config(config_file: Option<&Path>) -> Env {
        let local = config_file
            .and_then(Path::parent)
            .map(|dir| dir.join("themes"));
        let theme_dirs = local
            .into_iter()
            .chain([PathBuf::from(GHOSTTY_THEMES)])
            .collect();
        Env {
            home: home_dir(),
            theme_dirs,
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| home.is_absolute())
}

/// Where the config file lives: $XDG_CONFIG_HOME/tern/config, or
/// ~/.config/tern/config. None when neither variable leads anywhere.
pub fn default_path() -> Option<PathBuf> {
    default_path_in(std::env::var_os("XDG_CONFIG_HOME"), home_dir())
}

fn default_path_in(xdg_config_home: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    // The XDG base directory spec says to ignore a relative path.
    let base = xdg_config_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home.map(|home| home.join(".config")))?;
    Some(base.join("tern").join("config"))
}

/// Splits command-line arguments into config overrides, each --key=value
/// becoming (key, value) for [Config::load](crate::Config::load), and the
/// other arguments, both in order.
pub fn cli_overrides<I>(args: I) -> (Vec<(String, String)>, Vec<String>)
where
    I: IntoIterator<Item = String>,
{
    let mut overrides = Vec::new();
    let mut rest = Vec::new();
    for arg in args {
        let pair = arg
            .strip_prefix("--")
            .and_then(|setting| setting.split_once('='))
            .filter(|(key, _)| !key.is_empty())
            .map(|(key, value)| (key.to_owned(), value.to_owned()));
        match pair {
            Some(pair) => overrides.push(pair),
            None => rest.push(arg),
        }
    }
    (overrides, rest)
}

/// A file's bytes; None when it does not exist.
pub(crate) fn read_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    // Checked before opening: opening a FIFO would block.
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot be read: {e}")),
    };
    if !metadata.is_file() {
        return Err("is not a regular file".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!("is larger than {} KiB", MAX_FILE_BYTES / 1024));
    }
    let file = fs::File::open(path).map_err(|e| format!("cannot be read: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot be read: {e}"))?;
    Ok(Some(bytes))
}

/// Replaces a leading ~ with the home directory.
pub(crate) fn expand_home(path: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    let Some(rest) = path.strip_prefix('~') else {
        return Ok(PathBuf::from(path));
    };
    let rest = match rest.strip_prefix('/') {
        Some(rest) => rest,
        None if rest.is_empty() => rest,
        None => {
            return Err(format!(
                "cannot expand \"{path}\": only ~ and ~/ are supported"
            ))
        }
    };
    let home =
        home.ok_or_else(|| format!("cannot expand \"{path}\": the home directory is unknown"))?;
    Ok(if rest.is_empty() {
        home.to_path_buf()
    } else {
        home.join(rest)
    })
}

/// The files a theme setting may refer to, in the order to try them.
fn theme_candidates(value: &str, env: &Env) -> Result<Vec<PathBuf>, String> {
    if value.starts_with('~') || value.starts_with('/') {
        return Ok(vec![expand_home(value, env.home.as_deref())?]);
    }
    if value.contains('/') || value == "." || value == ".." {
        return Err(format!(
            "\"{value}\" is neither a theme name nor an absolute path"
        ));
    }
    Ok(env.theme_dirs.iter().map(|dir| dir.join(value)).collect())
}

/// Finds and reads the theme a theme setting names. Problems are reported at
/// the setting's line, except those inside the theme file, which are reported
/// at their own line of that file.
pub(crate) fn load_theme(
    value: &str,
    source: &Source,
    line: usize,
    env: &Env,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Colors> {
    let mut report = |message: String| {
        diagnostics.push(Diagnostic {
            source: source.clone(),
            line,
            message: format!("theme: {message}"),
        })
    };
    let candidates = match theme_candidates(value, env) {
        Ok(candidates) => candidates,
        Err(message) => {
            report(message);
            return None;
        }
    };
    for path in &candidates {
        match read_file(path) {
            Ok(Some(bytes)) => return Some(parse_theme(&bytes, path, diagnostics)),
            Ok(None) => continue,
            Err(message) => {
                report(format!("{}: {message}", path.display()));
                return None;
            }
        }
    }
    let tried: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
    report(format!(
        "\"{value}\" not found (looked for {})",
        tried.join(", ")
    ));
    None
}

/// The colors a theme file sets.
fn parse_theme(bytes: &[u8], path: &Path, diagnostics: &mut Vec<Diagnostic>) -> Colors {
    let mut colors = Colors::default();
    let source = Source::File(path.to_path_buf());
    for (line, setting) in settings(bytes) {
        let problem = match setting {
            Err(message) => Some(message),
            Ok((key, value)) => match colors.set(key, value) {
                Some(Ok(())) => None,
                Some(Err(message)) => Some(format!("{key}: {message}")),
                None => Some(format!(
                    "\"{key}\" cannot be set in a theme, only colors and palette entries"
                )),
            },
        };
        if let Some(message) = problem {
            diagnostics.push(Diagnostic {
                source: source.clone(),
                line,
                message,
            });
        }
    }
    colors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env {
            home: Some(PathBuf::from("/Users/test")),
            theme_dirs: vec![
                PathBuf::from("/conf/tern/themes"),
                PathBuf::from("/ghostty/themes"),
            ],
        }
    }

    #[test]
    fn default_path_prefers_an_absolute_xdg_config_home() {
        let home = Some(PathBuf::from("/Users/test"));
        assert_eq!(
            default_path_in(Some("/xdg".into()), home.clone()),
            Some(PathBuf::from("/xdg/tern/config"))
        );
        assert_eq!(
            default_path_in(None, home.clone()),
            Some(PathBuf::from("/Users/test/.config/tern/config"))
        );
        // Relative and empty values are ignored, as the XDG spec says.
        assert_eq!(
            default_path_in(Some("relative".into()), home.clone()),
            Some(PathBuf::from("/Users/test/.config/tern/config"))
        );
        assert_eq!(
            default_path_in(Some("".into()), home),
            Some(PathBuf::from("/Users/test/.config/tern/config"))
        );
        assert_eq!(default_path_in(None, None), None);
        assert_eq!(default_path_in(Some("x".into()), None), None);
    }

    #[test]
    fn default_path_is_absolute_here() {
        // This process runs with a home directory.
        let path = default_path().expect("a default path");
        assert!(path.is_absolute());
        assert!(path.ends_with("tern/config"));
    }

    #[test]
    fn home_expansion() {
        let home = Some(Path::new("/Users/test"));
        assert_eq!(expand_home("~", home), Ok(PathBuf::from("/Users/test")));
        assert_eq!(expand_home("~/", home), Ok(PathBuf::from("/Users/test")));
        assert_eq!(
            expand_home("~/a/b", home),
            Ok(PathBuf::from("/Users/test/a/b"))
        );
        assert_eq!(expand_home("/a/~", home), Ok(PathBuf::from("/a/~")));
        assert_eq!(expand_home("rel", home), Ok(PathBuf::from("rel")));
        assert!(expand_home("~bob/x", home)
            .unwrap_err()
            .contains("only ~ and ~/"));
        assert!(expand_home("~/x", None)
            .unwrap_err()
            .contains("home directory is unknown"));
    }

    #[test]
    fn theme_names_are_looked_up_in_order() {
        assert_eq!(
            theme_candidates("Dracula", &env()),
            Ok(vec![
                PathBuf::from("/conf/tern/themes/Dracula"),
                PathBuf::from("/ghostty/themes/Dracula"),
            ])
        );
        assert_eq!(
            theme_candidates("Builtin Solarized Dark", &env()),
            Ok(vec![
                PathBuf::from("/conf/tern/themes/Builtin Solarized Dark"),
                PathBuf::from("/ghostty/themes/Builtin Solarized Dark"),
            ])
        );
    }

    #[test]
    fn theme_paths_are_used_as_given() {
        assert_eq!(
            theme_candidates("/a/b", &env()),
            Ok(vec![PathBuf::from("/a/b")])
        );
        assert_eq!(
            theme_candidates("~/themes/mine", &env()),
            Ok(vec![PathBuf::from("/Users/test/themes/mine")])
        );
        for bad in ["a/b", "./x", ".", "..", "../x"] {
            let error = theme_candidates(bad, &env()).unwrap_err();
            assert!(
                error.contains("neither a theme name nor an absolute path"),
                "{bad}: {error}"
            );
        }
        assert!(theme_candidates("~bob/x", &env()).is_err());
    }

    #[test]
    fn theme_files_report_their_own_lines() {
        let text = "# A theme\nbackground = #101010\nfont-size = 12\n\npalette = 3=#zzzzzz\nforeground=#eeeeee\nnot a setting\n";
        let mut diagnostics = Vec::new();
        let path = Path::new("/themes/t");
        let colors = parse_theme(text.as_bytes(), path, &mut diagnostics);
        let theme = crate::colors::compose(Some(&colors), &Colors::default(), false);
        assert_eq!((theme.bg, theme.fg), (0x101010, 0xeeeeee));
        let found: Vec<(Source, usize)> = diagnostics
            .iter()
            .map(|d| (d.source.clone(), d.line))
            .collect();
        let file = Source::File(path.to_path_buf());
        assert_eq!(found, vec![(file.clone(), 3), (file.clone(), 5), (file, 7)]);
        assert!(diagnostics[0]
            .message
            .contains("\"font-size\" cannot be set in a theme"));
        assert!(diagnostics[1]
            .message
            .starts_with("palette: expected a color"));
    }

    #[test]
    fn command_line_arguments_split_into_overrides_and_the_rest() {
        let args = [
            "--font-size=14",
            "-psn_0_123",
            "--theme=",
            "--keybind=cmd+x=text:a=b",
            "--help",
            "--=x",
            "plain",
            "--a=b=c",
        ]
        .map(String::from);
        let (overrides, rest) = cli_overrides(args);
        let pairs: Vec<(&str, &str)> = overrides
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("font-size", "14"),
                ("theme", ""),
                ("keybind", "cmd+x=text:a=b"),
                ("a", "b=c")
            ]
        );
        assert_eq!(rest, ["-psn_0_123", "--help", "--=x", "plain"]);
    }
}
