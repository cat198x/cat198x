//! ROM filtering utilities
//!
//! This module provides filtering capabilities including 1G1R (One Game One ROM)
//! which helps reduce regional variants to a single preferred version per game.
//!
//! Supports both No-Intro and TOSEC naming conventions.

pub mod parser;
pub mod preferences;

pub use parser::*;
pub use preferences::*;
