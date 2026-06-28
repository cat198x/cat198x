//! Cat198x - A cross-platform CLI for managing retro gaming ROM collections
//!
//! This library provides the core functionality for managing ROM collections,
//! including DAT file parsing, file scanning, and database operations.

pub mod archive;
pub mod cli;
pub mod config;
pub mod dat;
pub mod db;
pub mod error;
pub mod filter;
pub mod ops;
pub mod plan;
pub mod scanner;
pub mod util;

// Re-export commonly used types at crate root for convenience
pub use dat::DatSourceType;

// Backward-compatible CLI argument re-exports. The definitions live in
// `cli::args` so the crate root stays focused on domain modules.
pub use cli::args::{
    ConfigCommands, DatCommands, QuarantineCommands, SourceCommands, TorrentCommands,
};
