//! Tern, a terminal for macOS.

mod app;
mod automation;
mod clipboard;
mod input;
mod keymap;
mod links;
mod mainthread;
mod menu;
mod pane;
mod startup;
mod state;
mod view;
mod window;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use objc2::MainThreadMarker;

const USAGE: &str = "\
usage: tern [options] [path...] [-e command [args...]]

options:
  --config-file <path>   read the configuration from <path> instead of
                         ~/.config/tern/config
  --<key>=<value>        override one configuration setting
  -e <command...>        run <command> instead of the shell (takes the rest of the line)
  --no-activate          do not bring Tern to the front at launch
  --control <path>       accept automation commands on a Unix socket at <path>
  --pasteboard <name>    use a private pasteboard instead of the clipboard
  --log-startup          print how long startup took, and where the time went
  -h, --help             print this help
  -V, --version          print the version
";

/// What the command line asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub no_activate: bool,
    pub control: Option<PathBuf>,
    pub log_startup: bool,
    pub config_file: Option<PathBuf>,
    /// A named pasteboard to use instead of the general one.
    pub pasteboard: Option<String>,
    /// Directories or files passed on the command line to open in windows.
    pub paths: Vec<PathBuf>,
    /// The program and arguments given with -e; empty for the login shell.
    pub command: Vec<String>,
    /// --key=value configuration overrides, in order.
    pub overrides: Vec<(String, String)>,
}

#[derive(Debug, PartialEq)]
enum Cli {
    Run(Options),
    Help,
    Version,
}

/// The app's own options that take a value; every other --key=value is a
/// configuration override.
const VALUE_OPTIONS: [&str; 3] = ["control", "config-file", "pasteboard"];

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut opts = Options::default();
    let mut args: Vec<String> = args.into_iter().collect();
    // Everything after -e belongs to the command.
    if let Some(e) = args.iter().position(|a| a == "-e") {
        opts.command = args.split_off(e).split_off(1);
        if opts.command.is_empty() {
            return Err("-e needs a command".into());
        }
    }
    let (overrides, rest) = tern_config::cli_overrides(args);
    for (key, value) in overrides {
        if VALUE_OPTIONS.contains(&key.as_str()) {
            set_value(&mut opts, &key, Some(value))?;
        } else {
            opts.overrides.push((key, value));
        }
    }
    let mut rest = rest.into_iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "--no-activate" => opts.no_activate = true,
            "--log-startup" => opts.log_startup = true,
            // Launch Services may still add a process serial number argument
            // (-psn_...), as on the first launch of a downloaded app; it means
            // nothing here.
            _ if arg.starts_with("-psn_") => {}
            _ if !arg.starts_with('-') && looks_like_path(&arg) => {
                opts.paths.push(expand_cli_path(&arg));
            }
            _ => match arg.strip_prefix("--").filter(|k| VALUE_OPTIONS.contains(k)) {
                Some(key) => set_value(&mut opts, key, rest.next())?,
                None => return Err(format!("unknown option {arg:?}")),
            },
        }
    }
    Ok(Cli::Run(opts))
}

fn looks_like_path(arg: &str) -> bool {
    arg == "."
        || arg == ".."
        || arg == "~"
        || arg.starts_with('/')
        || arg.starts_with("./")
        || arg.starts_with("../")
        || arg.starts_with("~/")
        || std::path::Path::new(arg).exists()
}

fn expand_cli_path(arg: &str) -> PathBuf {
    if arg == "~" {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h);
        }
    } else if let Some(rel) = arg.strip_prefix("~/")
        && let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rel);
        }
    PathBuf::from(arg)
}

/// Sets an option that takes a value. An empty value is a mistake, not the
/// current directory or a pasteboard without a name.
fn set_value(opts: &mut Options, key: &str, value: Option<String>) -> Result<(), String> {
    let value = value
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("--{key} needs a value"))?;
    match key {
        "control" => opts.control = Some(value.into()),
        "config-file" => opts.config_file = Some(value.into()),
        _ => opts.pasteboard = Some(value),
    }
    Ok(())
}

/// When the kernel started this process, so startup time includes the
/// dynamic loader and everything before main.
fn process_start_time() -> Option<SystemTime> {
    // SAFETY: proc_bsdinfo is plain data, so all zeroes is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is a proc_bsdinfo of the size passed.
    let n = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (n == size).then(|| {
        SystemTime::UNIX_EPOCH
            + Duration::from_secs(info.pbi_start_tvsec)
            + Duration::from_micros(info.pbi_start_tvusec)
    })
}

fn main() -> ExitCode {
    let start = process_start_time().unwrap_or_else(SystemTime::now);
    let opts = match parse_args(std::env::args().skip(1)) {
        Ok(Cli::Run(opts)) => opts,
        Ok(Cli::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Cli::Version) => {
            println!("tern {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("tern: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("tern: must start on the main thread");
        return ExitCode::FAILURE;
    };
    match app::run(mtm, opts, start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tern: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    fn run(args: &[&str]) -> Options {
        match parse(args) {
            Ok(Cli::Run(o)) => o,
            other => panic!("{args:?} gave {other:?}"),
        }
    }

    #[test]
    fn no_arguments_gives_defaults() {
        assert_eq!(run(&[]), Options::default());
    }

    #[test]
    fn flags_and_values() {
        let o = run(&["--no-activate", "/tmp", "./src"]);
        assert_eq!(o.paths, [PathBuf::from("/tmp"), PathBuf::from("./src")]);
        let o = run(&[
            "--no-activate",
            "--control",
            "/tmp/s",
            "--log-startup",
            "--config-file=/tmp/c",
            "--pasteboard",
            "p",
            "--window-width=40",
            "--window-position-x=-10",
            "--quit-after-last-window-closed=true",
        ]);
        assert!(o.no_activate && o.log_startup);
        assert_eq!(o.control, Some(PathBuf::from("/tmp/s")));
        assert_eq!(o.config_file, Some(PathBuf::from("/tmp/c")));
        assert_eq!(o.pasteboard.as_deref(), Some("p"));
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(
            o.overrides,
            [
                pair("window-width", "40"),
                pair("window-position-x", "-10"),
                pair("quit-after-last-window-closed", "true"),
            ]
        );
        let o = run(&["--control=/tmp/x", "--pasteboard=q", "--config-file", "/c"]);
        assert_eq!(o.control, Some(PathBuf::from("/tmp/x")));
        assert_eq!(o.pasteboard.as_deref(), Some("q"));
        assert_eq!(o.config_file, Some(PathBuf::from("/c")));
        assert!(o.overrides.is_empty());
    }

    #[test]
    fn dash_e_takes_the_rest() {
        let o = run(&["--no-activate", "-e", "vim", "--no-activate", "--x=1"]);
        assert_eq!(o.command, ["vim", "--no-activate", "--x=1"]);
        assert!(o.overrides.is_empty());
        assert!(parse(&["-e"]).is_err());
    }

    #[test]
    fn errors() {
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["stray"]).is_err());
        assert!(parse(&["--control"]).is_err());
        assert!(parse(&["--control", ""]).is_err());
        assert!(parse(&["--control="]).is_err());
        assert!(parse(&["--config-file="]).is_err());
        assert!(parse(&["--config-file"]).is_err());
        assert!(parse(&["--pasteboard"]).is_err());
    }

    #[test]
    fn unknown_settings_are_left_to_the_configuration() {
        // The configuration reports them; they do not stop the app.
        assert_eq!(
            run(&["--nope=1"]).overrides,
            [("nope".to_string(), "1".to_string())]
        );
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse(&["--help"]), Ok(Cli::Help));
        assert_eq!(parse(&["-V"]), Ok(Cli::Version));
    }

    #[test]
    fn process_start_is_in_the_past() {
        let t = process_start_time().expect("proc_pidinfo works for our own pid");
        assert!(t <= SystemTime::now());
    }
}
