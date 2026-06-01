//! Database operations for Cat198x

mod schema;
pub mod collections;
pub mod config;
pub mod dats;
pub mod files;
pub mod quarantine;

pub use schema::Database;
