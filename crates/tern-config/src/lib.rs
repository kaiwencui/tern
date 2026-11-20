//! The configuration file, the built-in defaults and the key bindings.
//!
//! The file is lines of `key = value`, the same shape as Ghostty's. Parsing
//! never fails as a whole: unknown keys and bad values become [Diagnostic]s
//! and are skipped. Only the loaders touch the system: [Config::load],
//! [Config::parse] and [default_path] read the config file, theme files and
//! the environment variables that locate them.

mod action;
mod bindings;
mod colors;
mod config;
mod load;
mod template;

use std::fmt;

pub use action::{
    Action, Direction, FocusTarget, FontSizeChange, PromptDirection, ScrollTarget,
};
pub use bindings::{Bindings, Trigger};
pub use config::{Config, Diagnostic, OptionAsAlt, Source, WindowSaveState, WorkingDirectory};
pub use load::{cli_overrides, default_path};
pub use template::DEFAULT_CONFIG_TEMPLATE;

/// Why a trigger or an action could not be parsed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseError(String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}
