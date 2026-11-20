//! Tern's terminal engine: the screen, the scrollback and the selection, and
//! what a program's output does to them.
//!
//! [engine] is the whole of what Tern's session uses; the other modules are
//! public only for the crate's own tests.

#![warn(rust_2018_idioms, future_incompatible)]
#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]
#![cfg_attr(clippy, deny(warnings))]

pub mod ansi;
pub mod engine;
#[doc(hidden)]
pub mod event;
mod feed;
#[doc(hidden)]
pub mod grid;
#[doc(hidden)]
pub mod index;
mod parser;
#[doc(hidden)]
pub mod selection;
#[doc(hidden)]
pub mod term;

#[doc(hidden)]
pub use crate::parser::BareParser;
#[doc(hidden)]
pub use crate::term::Term;
